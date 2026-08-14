use super::*;

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn daemon_forwards_h3_http_absolute_form_to_a_tcp_origin_and_releases_udp_listener() {
    let origin = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("origin bind");
    let origin_address = origin.local_addr().expect("origin address");
    let origin_task = tokio::spawn(async move {
        let (mut stream, _) = origin.accept().await.expect("HTTP origin accept");
        let mut request = Vec::new();
        let mut buffer = [0; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).await.expect("HTTP origin request");
            assert_ne!(read, 0, "HTTP origin request ended before headers");
            request.extend_from_slice(&buffer[..read]);
        }
        assert!(request.starts_with(b"POST /h3 HTTP/1.1\r\n"));
        assert!(
            request
                .windows(b"host: ".len())
                .any(|window| window.eq_ignore_ascii_case(b"host: "))
        );
        let header_end = request
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("HTTP origin headers")
            + 4;
        while request.len() < header_end + 7 {
            let read = stream.read(&mut buffer).await.expect("HTTP origin body");
            assert_ne!(read, 0, "HTTP origin body ended early");
            request.extend_from_slice(&buffer[..read]);
        }
        assert_eq!(&request[header_end..header_end + 7], b"payload");
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nplain")
            .await
            .expect("HTTP origin response");

        let (mut stream, _) = origin.accept().await.expect("CONNECT origin accept");
        let mut request = [0; 4];
        stream
            .read_exact(&mut request)
            .await
            .expect("CONNECT origin payload");
        assert_eq!(&request, b"ping");
        stream
            .write_all(b"pong")
            .await
            .expect("CONNECT origin response");
    });

    let proxy_address = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
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
    config.forward_proxy_services.push(ForwardProxyService {
        name: "forward".into(),
        enabled_versions: vec![ForwardHttpVersion::H3],
        allow_absolute_form: true,
        tls_required: true,
        connect: ForwardConnectPolicy {
            enabled: true,
            allowed_ports: vec![origin_address.port()],
        },
        connect_udp: ForwardConnectPolicy::default(),
        peer_policy: ForwardPeerPolicy::default(),
        auth: None,
        access_policy: None,
        destination_policy: ForwardDestinationPolicy {
            deny_private: false,
            ..ForwardDestinationPolicy::default()
        },
        header_policy: ForwardHeaderPolicy::default(),
        connect_timeout_ms: 1_000,
        idle_timeout_ms: 1_000,
        lifetime_timeout_ms: 5_000,
        max_request_body_bytes: Some(64 * 1024),
        max_header_bytes: 8_192,
        max_connections: 4,
        resolver: ForwardResolverPolicy::default(),
        audit_mode: ForwardAuditMode::Off,
    });
    config.listeners.push(Listener {
        name: "forward".into(),
        bind: ListenerBind::Udp {
            address: proxy_address,
        },
        protocol: Protocol::ForwardHttp3,
        service: Some("forward".into()),
        tls_profile: Some("downstream".into()),
        proxy_protocol: None,
        max_connections: None,
        downstream_timeouts: DownstreamTimeoutPolicy::default(),
    });

    let config = config.validate().expect("valid forward H3 config");
    let server = process_support::ServerProcess::start(&config, None);
    let endpoint = client_endpoint(&fixture_support::fixture("ca-a.pem"), H3_ALPN)
        .expect("H3 client endpoint");
    let connection = connect(&endpoint, proxy_address, support::PROXY_SERVER_NAME).await;
    let (driver, mut sender) = h3::client::new(h3_quinn::Connection::new(connection))
        .await
        .expect("H3 client connection");
    let driver = drive_client(driver);
    let request = Request::builder()
        .method(Method::POST)
        .uri(format!("http://127.0.0.1:{}/h3", origin_address.port()))
        .header("content-length", "7")
        .body(())
        .expect("H3 absolute-form request");
    let mut stream = sender.send_request(request).await.expect("send H3 request");
    stream
        .send_data(Bytes::from_static(b"payload"))
        .await
        .expect("H3 request body");
    stream.finish().await.expect("finish H3 request");
    let response = stream.recv_response().await.expect("H3 response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(recv_chunk(&mut stream).await.as_ref(), b"plain");
    assert!(
        stream
            .recv_data()
            .await
            .expect("H3 response finish")
            .is_none()
    );

    let mut malformed = sender
        .send_request(
            Request::builder()
                .method(Method::GET)
                .uri(format!(
                    "http://127.0.0.1:{}/malformed",
                    origin_address.port()
                ))
                .header("content-length", "not-a-number")
                .body(())
                .expect("malformed H3 request"),
        )
        .await
        .expect("send malformed H3 request");
    malformed
        .finish()
        .await
        .expect("finish malformed H3 request");
    assert_eq!(
        malformed
            .recv_response()
            .await
            .expect("malformed H3 response")
            .status(),
        StatusCode::BAD_REQUEST
    );

    let mut oversized = sender
        .send_request(
            Request::builder()
                .method(Method::GET)
                .uri(format!(
                    "http://127.0.0.1:{}/oversized",
                    origin_address.port()
                ))
                .header("content-length", (64 * 1024 + 1).to_string())
                .body(())
                .expect("oversized H3 request"),
        )
        .await
        .expect("send oversized H3 request");
    oversized
        .finish()
        .await
        .expect("finish oversized H3 request");
    assert_eq!(
        oversized
            .recv_response()
            .await
            .expect("oversized H3 response")
            .status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );

    let mut connect = sender
        .send_request(
            Request::builder()
                .method(Method::CONNECT)
                .uri(origin_address.to_string())
                .body(())
                .expect("H3 CONNECT request"),
        )
        .await
        .expect("send H3 CONNECT");
    assert_eq!(
        connect
            .recv_response()
            .await
            .expect("H3 CONNECT response")
            .status(),
        StatusCode::OK
    );
    connect
        .send_data(Bytes::from_static(b"ping"))
        .await
        .expect("H3 CONNECT payload");
    assert_eq!(recv_chunk(&mut connect).await.as_ref(), b"pong");
    connect.finish().await.expect("finish H3 CONNECT");
    origin_task.await.expect("CONNECT origin task");

    drop(stream);
    drop(malformed);
    drop(oversized);
    drop(connect);
    drop(sender);
    endpoint.close(quinn::VarInt::from_u32(0), b"test complete");
    driver.await.expect("H3 driver task");
    server.shutdown_gracefully();
    let released = std::net::UdpSocket::bind(proxy_address).expect("UDP listener release");
    drop(released);
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn h3_cache_serves_a_second_absolute_form_get_without_origin_contact() {
    let origin = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("cache origin bind");
    let origin_address = origin.local_addr().expect("cache origin address");
    let origin_task = tokio::spawn(async move {
        let (mut stream, _) = origin.accept().await.expect("cache origin accept");
        let mut request = Vec::new();
        let mut buffer = [0; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).await.expect("cache origin read");
            assert_ne!(read, 0, "cache origin request ended before headers");
            request.extend_from_slice(&buffer[..read]);
        }
        assert!(request.starts_with(b"GET /cached HTTP/1.1\r\n"));
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nCache-Control: public, max-age=60\r\nContent-Length: 6\r\nConnection: close\r\n\r\ncached",
            )
            .await
            .expect("cache origin response");
        assert!(
            timeout(Duration::from_millis(300), origin.accept())
                .await
                .is_err(),
            "cache hit contacted the origin"
        );
    });

    let proxy_address = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("reserve cache UDP address")
        .local_addr()
        .expect("reserved cache UDP address");
    let key = fixture_support::private_key_fixture("proxy-a-key.pem");
    let mut config = support::empty_config();
    config
        .cache_stores
        .push(CacheStore::memory("memory", 1024 * 1024));
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
    config.forward_proxy_services.push(ForwardProxyService {
        name: "forward".into(),
        enabled_versions: vec![ForwardHttpVersion::H3],
        allow_absolute_form: true,
        tls_required: true,
        connect: ForwardConnectPolicy::default(),
        connect_udp: ForwardConnectPolicy::default(),
        peer_policy: ForwardPeerPolicy::default(),
        auth: None,
        access_policy: None,
        destination_policy: ForwardDestinationPolicy {
            deny_private: false,
            ..ForwardDestinationPolicy::default()
        },
        header_policy: ForwardHeaderPolicy {
            cache: Some(Box::new(HttpCachePolicy {
                store: "memory".into(),
                methods: vec!["GET".into(), "HEAD".into()],
                key_components: vec![
                    CacheKeyComponent::Scheme,
                    CacheKeyComponent::NormalizedHost,
                    CacheKeyComponent::PathAndQuery,
                ],
                use_origin_cache_control: true,
                default_ttl_ms: 60_000,
                status_ttls: Vec::new(),
                grace_ms: 30_000,
                keep_ms: 300_000,
                revalidate: true,
                collapsed_forwarding: true,
                stale_on: Vec::new(),
                bypass_request: Vec::new(),
                no_store_request: Vec::new(),
                no_store_response: Vec::new(),
                set_cookie_policy: oxiroute_config::CacheSetCookiePolicy::default(),
                authorization_policy: oxiroute_config::CacheAuthorizationPolicy::default(),
                vary_policy: oxiroute_config::CacheVaryPolicy::default(),
                surrogate_tags: None,
                purge_authorization: None,
            })),
            ..ForwardHeaderPolicy::default()
        },
        connect_timeout_ms: 1_000,
        idle_timeout_ms: 1_000,
        lifetime_timeout_ms: 5_000,
        max_request_body_bytes: Some(64 * 1024),
        max_header_bytes: 8_192,
        max_connections: 4,
        resolver: ForwardResolverPolicy::default(),
        audit_mode: ForwardAuditMode::Off,
    });
    config.listeners.push(Listener {
        name: "forward".into(),
        bind: ListenerBind::Udp {
            address: proxy_address,
        },
        protocol: Protocol::ForwardHttp3,
        service: Some("forward".into()),
        tls_profile: Some("downstream".into()),
        proxy_protocol: None,
        max_connections: None,
        downstream_timeouts: DownstreamTimeoutPolicy::default(),
    });

    let config = config.validate().expect("valid forward H3 config");
    let server = process_support::ServerProcess::start(&config, None);
    let endpoint = client_endpoint(&fixture_support::fixture("ca-a.pem"), H3_ALPN)
        .expect("cache H3 client endpoint");
    let connection = connect(&endpoint, proxy_address, support::PROXY_SERVER_NAME).await;
    let (driver, mut sender) = h3::client::new(h3_quinn::Connection::new(connection))
        .await
        .expect("cache H3 client connection");
    let driver = drive_client(driver);
    for _ in 0..2 {
        let request = Request::builder()
            .method(Method::GET)
            .uri(format!("http://{origin_address}/cached"))
            .body(())
            .expect("cache H3 request");
        let mut stream = sender
            .send_request(request)
            .await
            .expect("send cache H3 request");
        stream.finish().await.expect("finish cache H3 request");
        let response = stream.recv_response().await.expect("cache H3 response");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(recv_chunk(&mut stream).await.as_ref(), b"cached");
        assert!(
            stream
                .recv_data()
                .await
                .expect("cache H3 response finish")
                .is_none()
        );
    }

    origin_task.await.expect("cache origin task");
    drop(sender);
    endpoint.close(quinn::VarInt::from_u32(0), b"test complete");
    driver.await.expect("cache H3 driver task");
    server.shutdown_gracefully();
}
