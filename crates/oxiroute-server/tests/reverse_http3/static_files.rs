#[allow(clippy::wildcard_imports)]
use super::*;

#[tokio::test]
#[allow(clippy::too_many_lines)]
pub(super) async fn daemon_serves_bounded_reverse_h3_static_files_and_ranges() {
    let directory = tempfile::tempdir().expect("static directory");
    let root = directory.path().join("public");
    fs::create_dir(&root).expect("static root");
    fs::write(root.join("ok.txt"), b"0123456789").expect("static file");

    let listener_address = process_support::ports::unused_address();
    let key = fixture_support::private_key_fixture("proxy-a-key.pem");
    let mut config = support::empty_config();
    config.certificates.push(Certificate {
        name: "downstream".into(),
        dns_names: vec![support::PROXY_SERVER_NAME.into()],
        source: CertificateSource::Files {
            certificate_chain_path: fixture_support::fixture("proxy-a.pem"),
            private_key_path: key.path().to_path_buf(),
        },
    });
    config.tls_profiles.push(TlsProfile {
        name: "downstream".into(),
        certificates: vec!["downstream".into()],
        default_certificate: "downstream".into(),
        min_version: TlsVersion::Tls13,
        alpn: vec![AlpnProtocol::H3],
        policy: oxiroute_config::TlsPolicy::default(),
    });
    config.http_services.push(HttpService {
        drain_deadline_ms: None,
        name: "web".into(),
        routes: vec![HttpRoute {
            host: None,
            path: HttpPathSelector::SegmentPrefix { value: "/".into() },
            methods: Vec::new(),
            access_policy: None,
            policy: HttpRoutePolicy {
                max_request_body_bytes: Some(64 * 1024),
                request_buffering: true,
                ..HttpRoutePolicy::default()
            },
            action: HttpRouteAction::StaticFiles {
                root_directory: root,
                path_mapping: HttpStaticPathMapping::Root,
                index_files: vec!["index.html".into()],
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
                headers: Vec::new(),
                error_responses: Vec::new(),
            },
        }],
        automatic_response_headers: true,
        upstream_io_timeout_ms: 5_000,
        max_request_body_bytes: Some(64 * 1024),
        gzip: None,
        access_log: None,
    });
    config.listeners.push(Listener {
        name: "reverse".into(),
        bind: ListenerBind::Udp {
            address: listener_address,
        },
        protocol: Protocol::Http3,
        service: Some("web".into()),
        tls_profile: Some("downstream".into()),
        proxy_protocol: None,
        max_connections: Some(8),
        downstream_timeouts: DownstreamTimeoutPolicy::default(),
    });

    let config = config
        .validate()
        .expect("valid reverse H3 static configuration");
    let server = process_support::ServerProcess::start(&config, None);
    let endpoint = client_endpoint().expect("H3 client endpoint");
    let connection = timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(connecting) = endpoint.connect(listener_address, support::PROXY_SERVER_NAME)
                && let Ok(connection) = connecting.await
            {
                break connection;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("H3 daemon connection timeout");
    let (driver, mut sender) = h3::client::new(h3_quinn::Connection::new(connection))
        .await
        .expect("H3 client connection");
    let driver = drive_client(driver);

    let mut success = sender
        .send_request(
            Request::builder()
                .method(Method::GET)
                .uri("https://example.test/ok.txt")
                .body(())
                .expect("static GET"),
        )
        .await
        .expect("send static GET");
    success.finish().await.expect("finish static GET");
    let response = success.recv_response().await.expect("static GET response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[http::header::CONTENT_LENGTH], "10");
    let etag = response.headers()[http::header::ETAG].clone();
    let last_modified = response.headers()[http::header::LAST_MODIFIED].clone();
    assert_eq!(recv_body(&mut success).await.as_ref(), b"0123456789");

    let mut not_modified = sender
        .send_request(
            Request::builder()
                .method(Method::GET)
                .uri("https://example.test/ok.txt")
                .header(http::header::IF_NONE_MATCH, etag.clone())
                .body(())
                .expect("conditional static GET"),
        )
        .await
        .expect("send conditional static GET");
    not_modified
        .finish()
        .await
        .expect("finish conditional static GET");
    let response = not_modified
        .recv_response()
        .await
        .expect("conditional static GET response");
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(response.headers()[http::header::ETAG], etag);
    assert_eq!(
        response.headers()[http::header::LAST_MODIFIED],
        last_modified
    );
    assert!(recv_body(&mut not_modified).await.is_empty());

    let mut precondition_failed = sender
        .send_request(
            Request::builder()
                .method(Method::GET)
                .uri("https://example.test/ok.txt")
                .header(http::header::IF_MATCH, "\"stale\"")
                .body(())
                .expect("precondition static GET"),
        )
        .await
        .expect("send precondition static GET");
    precondition_failed
        .finish()
        .await
        .expect("finish precondition static GET");
    let response = precondition_failed
        .recv_response()
        .await
        .expect("precondition static GET response");
    assert_eq!(response.status(), StatusCode::PRECONDITION_FAILED);
    assert_eq!(response.headers()[http::header::ETAG], etag);
    assert_eq!(
        response.headers()[http::header::LAST_MODIFIED],
        last_modified
    );
    assert!(recv_body(&mut precondition_failed).await.is_empty());

    let mut head = sender
        .send_request(
            Request::builder()
                .method(Method::HEAD)
                .uri("https://example.test/ok.txt")
                .body(())
                .expect("static HEAD"),
        )
        .await
        .expect("send static HEAD");
    head.finish().await.expect("finish static HEAD");
    let response = head.recv_response().await.expect("static HEAD response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[http::header::CONTENT_LENGTH], "10");
    assert!(recv_body(&mut head).await.is_empty());

    let mut range = sender
        .send_request(
            Request::builder()
                .method(Method::GET)
                .uri("https://example.test/ok.txt")
                .header(http::header::RANGE, "bytes=2-5")
                .body(())
                .expect("static range"),
        )
        .await
        .expect("send static range");
    range.finish().await.expect("finish static range");
    let response = range.recv_response().await.expect("static range response");
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        response.headers()[http::header::CONTENT_RANGE],
        "bytes 2-5/10"
    );
    assert_eq!(recv_body(&mut range).await.as_ref(), b"2345");

    let mut stale_if_range = sender
        .send_request(
            Request::builder()
                .method(Method::GET)
                .uri("https://example.test/ok.txt")
                .header(http::header::RANGE, "bytes=2-5")
                .header(http::header::IF_RANGE, "\"stale\"")
                .body(())
                .expect("stale If-Range static GET"),
        )
        .await
        .expect("send stale If-Range static GET");
    stale_if_range
        .finish()
        .await
        .expect("finish stale If-Range static GET");
    let response = stale_if_range
        .recv_response()
        .await
        .expect("stale If-Range static GET response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(recv_body(&mut stale_if_range).await.as_ref(), b"0123456789");

    let mut config = config.to_draft();
    config.http_services[0].routes[0].action = HttpRouteAction::FixedResponse {
        status: 200,
        body: "reloaded".into(),
        headers: Vec::new(),
    };
    process_support::write_config(
        &server.config_path,
        &config.validate().expect("valid replacement H3 config"),
    );
    timeout(Duration::from_secs(10), async {
        loop {
            let Ok(connecting) = endpoint.connect(listener_address, support::PROXY_SERVER_NAME)
            else {
                sleep(Duration::from_millis(25)).await;
                continue;
            };
            let Ok(connection) = connecting.await else {
                sleep(Duration::from_millis(25)).await;
                continue;
            };
            let Ok((driver, mut sender)) =
                h3::client::new(h3_quinn::Connection::new(connection)).await
            else {
                sleep(Duration::from_millis(25)).await;
                continue;
            };
            let driver = drive_client(driver);
            let Ok(mut stream) = sender
                .send_request(
                    Request::builder()
                        .method(Method::GET)
                        .uri("https://example.test/reloaded")
                        .body(())
                        .expect("reloaded request"),
                )
                .await
            else {
                let _ = timeout(Duration::from_secs(1), driver).await;
                sleep(Duration::from_millis(25)).await;
                continue;
            };
            let _ = stream.finish().await;
            let Ok(response) = stream.recv_response().await else {
                drop(stream);
                drop(sender);
                let _ = timeout(Duration::from_secs(1), driver).await;
                sleep(Duration::from_millis(25)).await;
                continue;
            };
            let body = recv_body(&mut stream).await;
            drop(stream);
            drop(sender);
            let _ = timeout(Duration::from_secs(1), driver).await;
            if response.status() == StatusCode::OK && body.as_ref() == b"reloaded" {
                break;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("H3 generation reload timeout");

    let idle_connection = timeout(Duration::from_secs(10), async {
        loop {
            let Ok(connecting) = endpoint.connect(listener_address, support::PROXY_SERVER_NAME)
            else {
                sleep(Duration::from_millis(25)).await;
                continue;
            };
            if let Ok(connection) = connecting.await {
                break connection;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("idle H3 connection timeout");
    let (idle_driver, mut idle_sender) =
        h3::client::new(h3_quinn::Connection::new(idle_connection))
            .await
            .expect("idle H3 client connection");
    let idle_driver = drive_client(idle_driver);

    let shutdown = tokio::task::spawn_blocking(move || server.shutdown_gracefully());
    timeout(Duration::from_secs(10), shutdown)
        .await
        .expect("H3 graceful shutdown timeout")
        .expect("H3 graceful shutdown task");
    assert!(
        sender
            .send_request(
                Request::builder()
                    .method(Method::GET)
                    .uri("https://example.test/after-goaway")
                    .body(())
                    .expect("post-GOAWAY request"),
            )
            .await
            .is_err(),
        "H3 accepted a request after GOAWAY"
    );
    assert!(
        idle_sender
            .send_request(
                Request::builder()
                    .method(Method::GET)
                    .uri("https://example.test/after-goaway-idle")
                    .body(())
                    .expect("post-GOAWAY idle request"),
            )
            .await
            .is_err(),
        "H3 accepted a first request after GOAWAY"
    );

    drop(success);
    drop(head);
    drop(range);
    drop(sender);
    drop(idle_sender);
    endpoint.close(quinn::VarInt::from_u32(0), b"test complete");
    driver.await.expect("H3 driver task");
    idle_driver.await.expect("idle H3 driver task");
}
