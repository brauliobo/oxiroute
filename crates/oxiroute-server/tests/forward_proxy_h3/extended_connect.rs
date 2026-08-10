use super::*;

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn daemon_rejects_h3_extended_connect_without_tcp_or_h1_fallback() {
    let origin = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("origin bind");
    let origin_address = origin.local_addr().expect("origin address");
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
        enabled_versions: vec![ForwardHttpVersion::H1, ForwardHttpVersion::H3],
        allow_absolute_form: true,
        tls_required: true,
        connect: ForwardConnectPolicy {
            enabled: true,
            allowed_ports: vec![origin_address.port()],
        },
        connect_udp: ForwardConnectPolicy {
            enabled: true,
            allowed_ports: vec![origin_address.port()],
        },
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

    let server = process_support::ServerProcess::start(&config, None);
    let endpoint = client_endpoint(&fixture_support::fixture("ca-a.pem"), H3_ALPN)
        .expect("H3 client endpoint");
    let connection = connect(&endpoint, proxy_address, support::PROXY_SERVER_NAME).await;
    let mut client_builder = h3::client::builder();
    client_builder.enable_extended_connect(true);
    let (driver, mut sender) = client_builder
        .build(h3_quinn::Connection::new(connection))
        .await
        .expect("H3 client connection");
    let driver = drive_client(driver);
    let mut request = Request::builder()
        .method(Method::CONNECT)
        .uri(format!(
            "https://127.0.0.1:{}/.well-known/masque/udp",
            origin_address.port()
        ))
        .body(())
        .expect("H3 extended CONNECT request");
    request
        .extensions_mut()
        .insert(h3::ext::Protocol::CONNECT_UDP);
    let mut stream = sender
        .send_request(request)
        .await
        .expect("send H3 extended CONNECT");
    stream.finish().await.expect("finish H3 extended CONNECT");
    let response = stream
        .recv_response()
        .await
        .expect("H3 extended CONNECT response");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(
        stream
            .recv_data()
            .await
            .expect("H3 rejection body")
            .is_none()
    );
    assert!(
        timeout(Duration::from_millis(500), origin.accept())
            .await
            .is_err(),
        "H3 extended CONNECT opened a TCP tunnel"
    );

    drop(stream);
    drop(sender);
    endpoint.close(quinn::VarInt::from_u32(0), b"test complete");
    driver.await.expect("H3 driver task");
    server.shutdown_gracefully();
    let released = std::net::UdpSocket::bind(proxy_address).expect("UDP listener release");
    drop(released);
}
