#[allow(clippy::wildcard_imports)]
use super::*;

#[tokio::test]
pub(super) async fn daemon_rejects_reverse_h3_connections_at_the_listener_limit() {
    let listener_address = reserve_udp_address();
    let upstream_address = reserve_udp_address();
    let key = fixture_support::private_key_fixture("proxy-a-key.pem");
    let config = reverse_config(
        listener_address,
        key.path(),
        proxy_service(1),
        vec![h3_pool(upstream_address)],
        Some(1),
    );
    let config = config.validate().expect("valid reverse H3 config");
    let server = process_support::ServerProcess::start(&config, None);
    let endpoint = client_endpoint().expect("H3 client endpoint");
    let first_connection = connect_h3(&endpoint, listener_address).await;
    let (driver, mut sender) = h3::client::new(h3_quinn::Connection::new(first_connection))
        .await
        .expect("first H3 client connection");
    let driver = drive_client(driver);
    let mut first_request = sender
        .send_request(
            Request::builder()
                .method(Method::POST)
                .uri("https://example.test/first")
                .header(http::header::CONTENT_LENGTH, "1")
                .body(())
                .expect("pending first H3 request"),
        )
        .await
        .expect("first H3 request");
    sleep(Duration::from_millis(100)).await;
    assert!(
        timeout(Duration::from_millis(100), first_request.recv_response())
            .await
            .is_err(),
        "first H3 request completed before the connection-cap probe"
    );

    let second_connection = connect_h3(&endpoint, listener_address).await;
    let close = timeout(Duration::from_secs(30), second_connection.closed())
        .await
        .expect("second H3 connection was not rejected");
    assert!(
        matches!(
            close,
            quinn::ConnectionError::ApplicationClosed(reason)
                if reason.error_code == quinn::VarInt::from_u32(0x100)
        ),
        "second H3 connection was rejected without the listener-limit close"
    );

    drop(first_request);
    drop(sender);
    endpoint.close(quinn::VarInt::from_u32(0), b"test complete");
    timeout(Duration::from_secs(30), driver)
        .await
        .expect("first H3 driver did not stop")
        .expect("first H3 driver task");
    server.shutdown_gracefully();
}

#[tokio::test]
pub(super) async fn daemon_rejects_reverse_h3_request_bodies_over_the_configured_bound() {
    let listener_address = reserve_udp_address();
    let upstream_address = reserve_udp_address();
    let key = fixture_support::private_key_fixture("proxy-a-key.pem");
    let config = reverse_config(
        listener_address,
        key.path(),
        proxy_service(4),
        vec![h3_pool(upstream_address)],
        Some(8),
    );
    let config = config.validate().expect("valid reverse H3 config");
    let server = process_support::ServerProcess::start(&config, None);
    let endpoint = client_endpoint().expect("H3 client endpoint");
    let connection = connect_h3(&endpoint, listener_address).await;
    let (driver, mut sender) = h3::client::new(h3_quinn::Connection::new(connection))
        .await
        .expect("H3 client connection");
    let driver = drive_client(driver);
    let mut request = sender
        .send_request(
            Request::builder()
                .method(Method::POST)
                .uri("https://example.test/body")
                .header(http::header::CONTENT_LENGTH, "5")
                .body(())
                .expect("oversized body request"),
        )
        .await
        .expect("send oversized body request");
    let _ = request.finish().await;
    assert_eq!(
        request
            .recv_response()
            .await
            .expect("oversized body response")
            .status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    let _ = recv_body(&mut request).await;

    drop(request);
    drop(sender);
    endpoint.close(quinn::VarInt::from_u32(0), b"test complete");
    timeout(Duration::from_secs(30), driver)
        .await
        .expect("H3 driver did not stop")
        .expect("H3 driver task");
    server.shutdown_gracefully();
}

#[tokio::test]
pub(super) async fn daemon_rejects_oversized_headers_and_malformed_reverse_h3_frames() {
    let listener_address = reserve_udp_address();
    let key = fixture_support::private_key_fixture("proxy-a-key.pem");
    let config = reverse_config(
        listener_address,
        key.path(),
        fixed_service(),
        Vec::new(),
        Some(8),
    );
    let config = config.validate().expect("valid reverse H3 config");
    let server = process_support::ServerProcess::start(&config, None);
    let endpoint = client_endpoint().expect("H3 client endpoint");
    let connection = connect_h3(&endpoint, listener_address).await;
    let raw_connection = connection.clone();
    let (driver, mut sender) = h3::client::new(h3_quinn::Connection::new(connection))
        .await
        .expect("H3 client connection");
    let driver = drive_client(driver);
    let mut oversized = sender
        .send_request(
            Request::builder()
                .method(Method::GET)
                .uri("https://example.test/headers")
                .header("x-padding", "x".repeat(20_000))
                .body(())
                .expect("oversized header request"),
        )
        .await
        .expect("send oversized header request");
    let _ = oversized.finish().await;
    assert_eq!(
        oversized
            .recv_response()
            .await
            .expect("oversized header response")
            .status(),
        StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE
    );

    let (mut send, _recv) = raw_connection.open_bi().await.expect("malformed stream");
    let mut malformed = BytesMut::new();
    malformed.write_var(0);
    malformed.write_var(0);
    send.write_all(&malformed)
        .await
        .expect("malformed H3 frame");
    let expected_code =
        quinn::VarInt::from_u64(Code::H3_FRAME_UNEXPECTED.value()).expect("H3 error code");
    let close = timeout(Duration::from_secs(30), raw_connection.closed())
        .await
        .expect("malformed frame close timeout");
    assert!(matches!(
        close,
        quinn::ConnectionError::ApplicationClosed(reason)
            if reason.error_code == expected_code
    ));

    drop(oversized);
    drop(sender);
    endpoint.close(quinn::VarInt::from_u32(0), b"test complete");
    timeout(Duration::from_secs(30), driver)
        .await
        .expect("H3 driver did not stop")
        .expect("H3 driver task");
    server.shutdown_gracefully();
}

#[tokio::test]
pub(super) async fn daemon_rejects_reverse_h3_static_responses_over_the_body_bound() {
    let directory = tempfile::tempdir().expect("static directory");
    let root = directory.path().join("public");
    fs::create_dir(&root).expect("static root");
    let oversized_path = root.join("oversized.bin");
    fs::File::create(&oversized_path)
        .expect("oversized static file")
        .set_len(64 * 1024 * 1024 + 1)
        .expect("sparse oversized static file");

    let listener_address = reserve_udp_address();
    let key = fixture_support::private_key_fixture("proxy-a-key.pem");
    let config = reverse_config(
        listener_address,
        key.path(),
        static_service(root),
        Vec::new(),
        Some(8),
    );
    let config = config.validate().expect("valid reverse H3 config");
    let server = process_support::ServerProcess::start(&config, None);
    let endpoint = client_endpoint().expect("H3 client endpoint");
    let connection = connect_h3(&endpoint, listener_address).await;
    let (driver, mut sender) = h3::client::new(h3_quinn::Connection::new(connection))
        .await
        .expect("H3 client connection");
    let driver = drive_client(driver);
    let mut request = sender
        .send_request(
            Request::builder()
                .method(Method::GET)
                .uri("https://example.test/oversized.bin")
                .body(())
                .expect("oversized static request"),
        )
        .await
        .expect("send oversized static request");
    request
        .finish()
        .await
        .expect("finish oversized static request");
    assert_eq!(
        request
            .recv_response()
            .await
            .expect("oversized static response")
            .status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    let _ = recv_body(&mut request).await;

    drop(request);
    drop(sender);
    endpoint.close(quinn::VarInt::from_u32(0), b"test complete");
    timeout(Duration::from_secs(30), driver)
        .await
        .expect("H3 driver did not stop")
        .expect("H3 driver task");
    server.shutdown_gracefully();
}

#[tokio::test]
pub(super) async fn daemon_enforces_reverse_h3_quic_stream_and_concurrent_request_bounds() {
    let listener_address = reserve_udp_address();
    let upstream_address = reserve_udp_address();
    let key = fixture_support::private_key_fixture("proxy-a-key.pem");
    let config = reverse_config(
        listener_address,
        key.path(),
        proxy_service(1),
        vec![h3_pool(upstream_address)],
        Some(8),
    );
    let config = config.validate().expect("valid reverse H3 config");
    let server = process_support::ServerProcess::start(&config, None);
    let endpoint = client_endpoint().expect("H3 client endpoint");
    let connection = connect_h3(&endpoint, listener_address).await;
    let (driver, mut sender) = h3::client::new(h3_quinn::Connection::new(connection))
        .await
        .expect("H3 client connection");
    let driver = drive_client(driver);
    let mut streams = Vec::with_capacity(128);
    for index in 0..128 {
        streams.push(
            timeout(
                Duration::from_secs(2),
                sender.send_request(
                    Request::builder()
                        .method(Method::POST)
                        .uri(format!("https://example.test/pending/{index}"))
                        .header(http::header::CONTENT_LENGTH, "1")
                        .body(())
                        .expect("bounded pending request"),
                ),
            )
            .await
            .expect("H3 stream admission timeout")
            .expect("H3 stream admission failure"),
        );
    }
    sleep(Duration::from_millis(100)).await;

    let exhausted = timeout(
        Duration::from_millis(250),
        sender.send_request(fixed_request("/over-limit")),
    )
    .await;
    assert!(
        exhausted.is_err(),
        "H3 accepted work after exhausting the QUIC bidirectional stream bound"
    );

    drop(streams);
    drop(sender);
    endpoint.close(quinn::VarInt::from_u32(0), b"test complete");
    timeout(Duration::from_secs(30), driver)
        .await
        .expect("H3 driver did not stop")
        .expect("H3 driver task");
    server.shutdown_gracefully();
}
