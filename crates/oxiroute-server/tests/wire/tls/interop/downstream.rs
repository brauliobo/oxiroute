use std::{fs, os::unix::fs::PermissionsExt as _};

use bytes::Bytes;
use http::{Method, StatusCode};
use oxiroute_config::{
    AlpnProtocol, HttpLiteralHeader, HttpRouteAction, HttpStaticErrorResponse,
    HttpStaticMimePolicy, HttpStaticPathMapping, HttpVersionPolicy,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::{sleep, timeout},
};

use crate::support::{
    H2Client, PROXY_SERVER_NAME, PlainH1Origin, ProxyHarness, ReservedListener, TEST_TIMEOUT,
    h1_request, negotiated_alpn, proxy_config, tls_connect,
};

#[tokio::test]
async fn downstream_h2_upload_streams_to_plain_h1_upstream() {
    timeout(TEST_TIMEOUT, async {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("H2 upload origin bind");
        let origin_address = listener.local_addr().expect("H2 upload origin address");
        let origin = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("H2 upload origin accept");
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            while !request.windows(5).any(|window| window == b"0\r\n\r\n") {
                let read = stream
                    .read(&mut buffer)
                    .await
                    .expect("H2 upload origin read");
                assert!(read > 0, "H2 upload ended before the terminating chunk");
                request.extend_from_slice(&buffer[..read]);
            }
            let request_text = String::from_utf8_lossy(&request).to_ascii_lowercase();
            assert!(request_text.starts_with("post /upload http/1.1\r\n"));
            assert!(request_text.contains("transfer-encoding: chunked\r\n"));
            let (chunks, body) = parse_chunked_request_body(&request);
            assert!(
                chunks >= 2,
                "multiple H2 DATA frames were collapsed upstream"
            );
            assert_eq!(body, b"h2-multi-data-upload");
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\nupload-ok",
                )
                .await
                .expect("H2 upload origin response");
        });
        let reserved = ReservedListener::new();
        let config = proxy_config(
            reserved.address,
            origin_address,
            vec![AlpnProtocol::H2, AlpnProtocol::Http11],
            None,
            HttpVersionPolicy::default(),
        );
        let proxy = ProxyHarness::start(&config.validate(), reserved);

        let stream = tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"h2"])
            .await
            .expect("H2 upload client connection");
        let mut client = H2Client::from_tls(stream)
            .await
            .expect("H2 upload client handshake");
        let response = client
            .request_with_body_chunks(
                Method::POST,
                "/upload",
                vec![
                    Bytes::from_static(b"h2-"),
                    Bytes::from_static(b"multi-"),
                    Bytes::from_static(b"data-"),
                    Bytes::from_static(b"upload"),
                ],
            )
            .await
            .expect("H2 upload response");
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.body, b"upload-ok".as_slice());

        origin.await.expect("H2 upload origin task");
        client.finish().await;
        proxy.finish().await;
    })
    .await
    .expect("downstream H2 upload test timed out");
}

fn parse_chunked_request_body(request: &[u8]) -> (usize, Vec<u8>) {
    let mut cursor = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("H2 upload request header terminator")
        + 4;
    let mut chunks = 0;
    let mut body = Vec::new();
    loop {
        let line_end = request[cursor..]
            .windows(2)
            .position(|window| window == b"\r\n")
            .expect("H1 chunk size terminator")
            + cursor;
        let size = usize::from_str_radix(
            std::str::from_utf8(&request[cursor..line_end])
                .expect("ASCII H1 chunk size")
                .split(';')
                .next()
                .unwrap(),
            16,
        )
        .expect("hex H1 chunk size");
        cursor = line_end + 2;
        if size == 0 {
            return (chunks, body);
        }
        chunks += 1;
        body.extend_from_slice(&request[cursor..cursor + size]);
        cursor += size;
        assert_eq!(&request[cursor..cursor + 2], b"\r\n");
        cursor += 2;
    }
}

#[tokio::test]
async fn downstream_h2_executes_fixed_actions_with_head_semantics_without_an_upstream() {
    timeout(TEST_TIMEOUT, async {
        let unused_origin =
            std::net::TcpListener::bind("127.0.0.1:0").expect("reserve unused action origin");
        unused_origin
            .set_nonblocking(true)
            .expect("unused action origin nonblocking");
        let origin_address = unused_origin.local_addr().expect("unused origin address");
        let reserved = ReservedListener::new();
        let mut config = proxy_config(
            reserved.address,
            origin_address,
            vec![AlpnProtocol::H2],
            None,
            HttpVersionPolicy::default(),
        );
        config.http_services[0].routes[0].action = HttpRouteAction::FixedResponse {
            status: 200,
            body: "h2-fixed".into(),
            headers: vec![HttpLiteralHeader {
                name: "x-action".into(),
                value: "fixed".into(),
                always: true,
            }],
        };
        let proxy = ProxyHarness::start(&config.validate(), reserved);
        let stream = tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"h2"])
            .await
            .expect("H2 action TLS connect");
        let mut client = H2Client::from_tls(stream).await.expect("H2 action client");

        let get = client
            .request(Method::GET, "/fixed")
            .await
            .expect("H2 fixed GET");
        assert_eq!(get.status, StatusCode::OK);
        assert_eq!(get.headers.get("x-action").unwrap(), "fixed");
        assert_eq!(get.headers.get("content-length").unwrap(), "8");
        assert_eq!(get.body, "h2-fixed");

        let head = client
            .request(Method::HEAD, "/fixed")
            .await
            .expect("H2 fixed HEAD");
        assert_eq!(head.status, StatusCode::OK);
        assert_eq!(head.headers.get("content-length").unwrap(), "8");
        assert!(head.body.is_empty());

        client.finish().await;
        proxy.finish().await;
        assert!(unused_origin.accept().is_err());
    })
    .await
    .expect("H2 fixed action test timed out");
}

#[tokio::test]
async fn downstream_h2_serves_static_headers_and_internal_error_page() {
    timeout(TEST_TIMEOUT, async {
        let directory = tempfile::tempdir().expect("H2 static response directory");
        let root = directory.path().join("public");
        fs::create_dir(&root).expect("create H2 static response root");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
            .expect("secure H2 static response root");
        fs::write(root.join("ok.txt"), b"ok").expect("write H2 static success file");
        fs::write(root.join("404.html"), b"missing").expect("write H2 static error file");

        let unused_origin =
            std::net::TcpListener::bind("127.0.0.1:0").expect("reserve unused H2 static origin");
        unused_origin
            .set_nonblocking(true)
            .expect("unused H2 static origin nonblocking");
        let origin_address = unused_origin
            .local_addr()
            .expect("unused H2 static origin address");
        let reserved = ReservedListener::new();
        let mut config = proxy_config(
            reserved.address,
            origin_address,
            vec![AlpnProtocol::H2],
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
            mime: HttpStaticMimePolicy {
                default_type: Some("text/plain".into()),
                types: Vec::new(),
            },
            headers: vec![
                HttpLiteralHeader {
                    name: "x-selected-status".into(),
                    value: "no".into(),
                    always: false,
                },
                HttpLiteralHeader {
                    name: "x-always".into(),
                    value: "yes".into(),
                    always: true,
                },
            ],
            error_responses: vec![HttpStaticErrorResponse {
                statuses: vec![404],
                file: Some("404.html".into()),
                body: None,
                headers: vec![HttpLiteralHeader {
                    name: "x-error".into(),
                    value: "yes".into(),
                    always: true,
                }],
                internal_redirect: Some("/404.html".into()),
            }],
        };
        let proxy = ProxyHarness::start(&config.validate(), reserved);
        let stream = tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"h2"])
            .await
            .expect("H2 static response TLS connect");
        let mut client = H2Client::from_tls(stream)
            .await
            .expect("H2 static response client");

        let success = client
            .request(Method::GET, "/ok.txt")
            .await
            .expect("H2 static success");
        assert_eq!(success.status, StatusCode::OK);
        assert_eq!(success.headers.get("x-selected-status").unwrap(), "no");
        assert_eq!(success.headers.get("x-always").unwrap(), "yes");
        assert!(!success.headers.contains_key("x-error"));
        assert_eq!(success.body.as_ref(), b"ok");

        let missing = client
            .request(Method::GET, "/missing")
            .await
            .expect("H2 static error page");
        assert_eq!(missing.status, StatusCode::NOT_FOUND);
        assert!(!missing.headers.contains_key("x-selected-status"));
        assert_eq!(missing.headers.get("x-always").unwrap(), "yes");
        assert_eq!(missing.headers.get("x-error").unwrap(), "yes");
        assert_eq!(missing.body.as_ref(), b"missing");

        client.finish().await;
        proxy.finish().await;
        assert!(unused_origin.accept().is_err());
    })
    .await
    .expect("static response H2 test timed out");
}

#[tokio::test]
async fn downstream_h2_only_rejects_no_alpn_and_h1_before_http_but_allows_h2() {
    timeout(TEST_TIMEOUT, async {
        let origin = PlainH1Origin::start(b"h2-only-downstream").await;
        let reserved = ReservedListener::new();
        let config = proxy_config(
            reserved.address,
            origin.address,
            vec![AlpnProtocol::H2],
            None,
            HttpVersionPolicy::default(),
        );
        let proxy = ProxyHarness::start(&config.validate(), reserved);

        // OpenSSL does not invoke the ALPN selection callback when the ClientHello omits ALPN, so
        // TLS can complete. The production listener wrapper must close before HTTP/1 parsing.
        let mut no_alpn = tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[])
            .await
            .expect("no-ALPN TLS handshake may complete");
        assert_eq!(negotiated_alpn(&no_alpn), None);
        assert!(
            h1_request(&mut no_alpn, "/must-not-parse", true)
                .await
                .is_err()
        );

        assert!(
            tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"http/1.1"],)
                .await
                .is_err(),
            "an explicit incompatible ALPN offer must receive a fatal handshake failure"
        );
        sleep(std::time::Duration::from_millis(25)).await;
        assert_eq!(origin.requests(), 0);
        assert_eq!(origin.http_bytes(), 0);

        let stream = tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"h2"])
            .await
            .expect("valid H2-only downstream handshake");
        assert_eq!(negotiated_alpn(&stream), Some(b"h2".as_slice()));
        let mut client = H2Client::from_tls(stream)
            .await
            .expect("valid H2-only downstream client");
        let response = client
            .request(Method::GET, "/valid-h2")
            .await
            .expect("valid H2-only proxied request");
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.body, b"h2-only-downstream".as_slice());
        origin.wait_for_requests(1).await;
        assert_eq!(origin.requests(), 1);

        client.finish().await;
        drop(no_alpn);
        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("downstream H2-only enforcement wire test timed out");
}
