#![allow(dead_code, unused_imports, clippy::duplicate_mod)]

#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/process.rs"]
mod process_support;
#[path = "support/mod.rs"]
mod support;

use std::{
    fs,
    io::{BufReader, Cursor},
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use bytes::{Buf as _, Bytes, BytesMut};
use h3::{client::RequestStream, error::Code, proto::coding::BufMutExt as _, server::Connection};
use http::{HeaderMap, HeaderValue, Method, Request, Response, StatusCode};
use oxiroute_config::{
    AlpnProtocol, Certificate, CertificateSource, Config, DownstreamTimeoutPolicy,
    HttpPathSelector, HttpProxyPolicy, HttpRoute, HttpRouteAction, HttpRoutePolicy, HttpService,
    HttpStaticMimePolicy, HttpStaticPathMapping, HttpVersion, HttpVersionPolicy, Listener,
    ListenerBind, Protocol, TlsProfile, TlsVersion, UpstreamAlgorithm, UpstreamPool, UpstreamTls,
    validate_config,
};
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use tokio::{
    io::AsyncWriteExt as _,
    time::{sleep, timeout},
};

const H3_ALPN: &[u8] = b"h3";

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn daemon_accepts_reverse_h3_and_reuses_the_http_service_pool() {
    let origin_endpoint =
        quinn::Endpoint::server(origin_server_config(), (Ipv4Addr::LOCALHOST, 0).into())
            .expect("origin endpoint");
    let origin_address = origin_endpoint.local_addr().expect("origin address");
    let origin_task = tokio::spawn(async move {
        let incoming = origin_endpoint.accept().await.expect("origin accept");
        let connection = incoming.await.expect("origin QUIC connection");
        let mut h3: Connection<_, Bytes> = h3::server::builder()
            .build(h3_quinn::Connection::new(connection))
            .await
            .expect("origin H3 connection");
        let resolver = h3
            .accept()
            .await
            .expect("origin H3 accept")
            .expect("origin H3 request");
        let (request, mut stream) = resolver.resolve_request().await.expect("origin request");
        assert_eq!(request.method(), Method::GET);
        assert_eq!(request.uri().path(), "/h3");
        assert!(
            stream
                .recv_data()
                .await
                .expect("origin request body")
                .is_none()
        );
        stream
            .send_response(
                Response::builder()
                    .status(StatusCode::OK)
                    .header("content-length", "4")
                    .body(())
                    .expect("origin response"),
            )
            .await
            .expect("origin response headers");
        stream
            .send_data(Bytes::from_static(b"pong"))
            .await
            .expect("origin response body");
        let mut trailers = HeaderMap::new();
        trailers.insert("x-origin-trailer", HeaderValue::from_static("complete"));
        stream
            .send_trailers(trailers)
            .await
            .expect("origin response trailers");
        stream.finish().await.expect("origin response finish");
        let _ = h3.accept().await;
    });

    let listener_address = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("reserve UDP address")
        .local_addr()
        .expect("reserved UDP address");
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
    config.upstream_pools.push(UpstreamPool {
        name: "origin".into(),
        servers: Vec::new(),
        endpoints: vec![support::socket_endpoint(origin_address)],
        algorithm: UpstreamAlgorithm::RoundRobin,
        health_check: None,
        passive_health: None,
        tls: Some(UpstreamTls {
            server_name: support::ORIGIN_SERVER_NAME.into(),
            ca_certificate_path: Some(fixture_support::fixture("ca-a.pem")),
        }),
        http_versions: HttpVersionPolicy {
            min: HttpVersion::Http3,
            max: HttpVersion::Http3,
        },
        queue_timeout_ms: None,
        connect_timeout_ms: None,
        server_timeout_ms: None,
        connection_reuse: oxiroute_config::UpstreamConnectionReuse::default(),
    });
    config.http_services.push(HttpService {
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
            action: HttpRouteAction::Proxy {
                upstream_pool: "origin".into(),
                policy: HttpProxyPolicy::default(),
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

    validate_config(&mut config).expect("valid reverse H3 configuration");
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
    let request = Request::builder()
        .method(Method::GET)
        .uri("http://example.test/h3")
        .body(())
        .expect("H3 reverse request");
    let mut stream = sender.send_request(request).await.expect("send H3 request");
    stream.finish().await.expect("finish H3 request");
    let response = stream.recv_response().await.expect("H3 response");
    let response_body = recv_chunk(&mut stream).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "response body: {response_body:?}"
    );
    assert_eq!(response_body.as_ref(), b"pong");
    assert_eq!(
        stream
            .recv_trailers()
            .await
            .expect("H3 response trailers")
            .expect("origin response trailers")
            .get("x-origin-trailer"),
        Some(&HeaderValue::from_static("complete"))
    );

    drop(stream);
    drop(sender);
    endpoint.close(quinn::VarInt::from_u32(0), b"test complete");
    driver.await.expect("H3 driver task");
    server.shutdown_gracefully();
    origin_task.await.expect("origin task");

    let released = std::net::UdpSocket::bind(listener_address).expect("UDP listener release");
    drop(released);
}

fn origin_server_config() -> quinn::ServerConfig {
    let mut certificate_reader = BufReader::new(
        fs::File::open(fixture_support::fixture("origin.pem")).expect("origin certificate"),
    );
    let certificates = rustls_pemfile::certs(&mut certificate_reader)
        .collect::<Result<Vec<_>, _>>()
        .expect("origin certificate chain");
    let mut key_reader = BufReader::new(
        fs::File::open(fixture_support::fixture("origin-key.pem")).expect("origin key"),
    );
    let private_key = rustls_pemfile::private_key(&mut key_reader)
        .expect("origin private key")
        .expect("origin private key block");
    let mut crypto =
        rustls::ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .with_no_client_auth()
            .with_single_cert(certificates, private_key)
            .expect("origin TLS identity");
    crypto.alpn_protocols = vec![H3_ALPN.to_vec()];
    crypto.max_early_data_size = 0;
    let mut config = quinn::ServerConfig::with_crypto(Arc::new(
        QuicServerConfig::try_from(crypto).expect("origin QUIC TLS configuration"),
    ));
    config.migration(false);
    config
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn daemon_serves_bounded_reverse_h3_static_files_and_ranges() {
    let directory = tempfile::tempdir().expect("static directory");
    let root = directory.path().join("public");
    fs::create_dir(&root).expect("static root");
    fs::write(root.join("ok.txt"), b"0123456789").expect("static file");

    let listener_address = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("reserve UDP address")
        .local_addr()
        .expect("reserved UDP address");
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

    validate_config(&mut config).expect("valid reverse H3 static configuration");
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
    assert_eq!(recv_body(&mut success).await.as_ref(), b"0123456789");

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

    config.http_services[0].routes[0].action = HttpRouteAction::FixedResponse {
        status: 200,
        body: "reloaded".into(),
        headers: Vec::new(),
    };
    process_support::write_config(&server.config_path, &config);
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

#[tokio::test]
async fn daemon_rejects_reverse_h3_connections_at_the_listener_limit() {
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
    let close = timeout(Duration::from_secs(2), second_connection.closed())
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
    timeout(Duration::from_secs(2), driver)
        .await
        .expect("first H3 driver did not stop")
        .expect("first H3 driver task");
    server.shutdown_gracefully();
}

#[tokio::test]
async fn daemon_rejects_reverse_h3_request_bodies_over_the_configured_bound() {
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
    timeout(Duration::from_secs(2), driver)
        .await
        .expect("H3 driver did not stop")
        .expect("H3 driver task");
    server.shutdown_gracefully();
}

#[tokio::test]
async fn daemon_rejects_oversized_headers_and_malformed_reverse_h3_frames() {
    let listener_address = reserve_udp_address();
    let key = fixture_support::private_key_fixture("proxy-a-key.pem");
    let config = reverse_config(
        listener_address,
        key.path(),
        fixed_service(),
        Vec::new(),
        Some(8),
    );
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
    let close = timeout(Duration::from_secs(2), raw_connection.closed())
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
    timeout(Duration::from_secs(2), driver)
        .await
        .expect("H3 driver did not stop")
        .expect("H3 driver task");
    server.shutdown_gracefully();
}

#[tokio::test]
async fn daemon_rejects_reverse_h3_static_responses_over_the_body_bound() {
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
    timeout(Duration::from_secs(2), driver)
        .await
        .expect("H3 driver did not stop")
        .expect("H3 driver task");
    server.shutdown_gracefully();
}

#[tokio::test]
async fn daemon_enforces_reverse_h3_quic_stream_and_concurrent_request_bounds() {
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
    timeout(Duration::from_secs(2), driver)
        .await
        .expect("H3 driver did not stop")
        .expect("H3 driver task");
    server.shutdown_gracefully();
}

fn reserve_udp_address() -> SocketAddr {
    std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("reserve UDP address")
        .local_addr()
        .expect("reserved UDP address")
}

fn reverse_config(
    listener_address: SocketAddr,
    key_path: &Path,
    service: HttpService,
    upstream_pools: Vec<UpstreamPool>,
    max_connections: Option<u64>,
) -> Config {
    let mut config = support::empty_config();
    config.certificates.push(Certificate {
        name: "downstream".into(),
        dns_names: vec![support::PROXY_SERVER_NAME.into()],
        source: CertificateSource::Files {
            certificate_chain_path: fixture_support::fixture("proxy-a.pem"),
            private_key_path: key_path.to_path_buf(),
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
    config.upstream_pools = upstream_pools;
    config.http_services.push(service);
    config.listeners.push(Listener {
        name: "reverse".into(),
        bind: ListenerBind::Udp {
            address: listener_address,
        },
        protocol: Protocol::Http3,
        service: Some("web".into()),
        tls_profile: Some("downstream".into()),
        proxy_protocol: None,
        max_connections,
        downstream_timeouts: DownstreamTimeoutPolicy::default(),
    });
    validate_config(&mut config).expect("valid reverse H3 test configuration");
    config
}

fn fixed_service() -> HttpService {
    reverse_service(HttpRouteAction::FixedResponse {
        status: 200,
        body: "ok".into(),
        headers: Vec::new(),
    })
}

fn proxy_service(max_request_body_bytes: u64) -> HttpService {
    let mut service = reverse_service(HttpRouteAction::Proxy {
        upstream_pool: "origin".into(),
        policy: HttpProxyPolicy::default(),
    });
    service.max_request_body_bytes = Some(max_request_body_bytes);
    service.routes[0].policy.max_request_body_bytes = Some(max_request_body_bytes);
    service
}

fn reverse_service(action: HttpRouteAction) -> HttpService {
    HttpService {
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
            action,
        }],
        automatic_response_headers: true,
        upstream_io_timeout_ms: 5_000,
        max_request_body_bytes: Some(64 * 1024),
        gzip: None,
        access_log: None,
    }
}

fn static_service(root: PathBuf) -> HttpService {
    reverse_service(HttpRouteAction::StaticFiles {
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
            default_type: Some("application/octet-stream".into()),
            types: Vec::new(),
        },
        headers: Vec::new(),
        error_responses: Vec::new(),
    })
}

fn h3_pool(address: SocketAddr) -> UpstreamPool {
    UpstreamPool {
        name: "origin".into(),
        servers: Vec::new(),
        endpoints: vec![support::socket_endpoint(address)],
        algorithm: UpstreamAlgorithm::RoundRobin,
        health_check: None,
        passive_health: None,
        tls: Some(UpstreamTls {
            server_name: support::ORIGIN_SERVER_NAME.into(),
            ca_certificate_path: Some(fixture_support::fixture("ca-a.pem")),
        }),
        http_versions: HttpVersionPolicy {
            min: HttpVersion::Http3,
            max: HttpVersion::Http3,
        },
        queue_timeout_ms: None,
        connect_timeout_ms: None,
        server_timeout_ms: None,
        connection_reuse: oxiroute_config::UpstreamConnectionReuse::default(),
    }
}

fn fixed_request(path: &str) -> Request<()> {
    Request::builder()
        .method(Method::GET)
        .uri(format!("https://example.test{path}"))
        .body(())
        .expect("fixed H3 request")
}

async fn connect_h3(endpoint: &quinn::Endpoint, listener_address: SocketAddr) -> quinn::Connection {
    timeout(Duration::from_secs(10), async {
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
    .expect("H3 daemon connection timeout")
}

fn client_endpoint() -> std::io::Result<quinn::Endpoint> {
    let mut roots = rustls::RootCertStore::empty();
    let ca = fs::read(fixture_support::fixture("ca-a.pem"))?;
    for certificate in rustls_pemfile::certs(&mut Cursor::new(ca)) {
        roots
            .add(certificate.map_err(std::io::Error::other)?)
            .map_err(std::io::Error::other)?;
    }
    let mut crypto = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    crypto.alpn_protocols = vec![H3_ALPN.to_vec()];
    let config = quinn::ClientConfig::new(Arc::new(
        QuicClientConfig::try_from(crypto).map_err(std::io::Error::other)?,
    ));
    let mut endpoint = quinn::Endpoint::client((Ipv4Addr::LOCALHOST, 0).into())?;
    endpoint.set_default_client_config(config);
    Ok(endpoint)
}

async fn recv_chunk<S>(stream: &mut RequestStream<S, Bytes>) -> Bytes
where
    S: h3::quic::BidiStream<Bytes>,
{
    let mut chunk = stream
        .recv_data()
        .await
        .expect("H3 response body")
        .expect("H3 response data");
    chunk.copy_to_bytes(chunk.remaining())
}

async fn recv_body<S>(stream: &mut RequestStream<S, Bytes>) -> Bytes
where
    S: h3::quic::RecvStream,
{
    let mut body = Vec::new();
    while let Some(mut chunk) = stream.recv_data().await.expect("H3 response body") {
        let length = chunk.remaining();
        body.extend_from_slice(&chunk.copy_to_bytes(length));
    }
    Bytes::from(body)
}

fn drive_client<C>(mut driver: h3::client::Connection<C, Bytes>) -> tokio::task::JoinHandle<()>
where
    C: h3::quic::Connection<Bytes> + Send + 'static,
    C::SendStream: Send,
    C::RecvStream: Send,
{
    tokio::spawn(async move {
        let _ = std::future::poll_fn(|context| driver.poll_close(context)).await;
    })
}
