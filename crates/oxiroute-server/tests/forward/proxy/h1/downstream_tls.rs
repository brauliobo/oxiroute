use oxiroute_config::{
    AlpnProtocol, Certificate, CertificateSource, ForwardAuditMode, ForwardConnectPolicy,
    ForwardDestinationPolicy, ForwardHeaderPolicy, ForwardHttpVersion, ForwardPeerPolicy,
    ForwardProxyAuth, ForwardProxyService, ForwardResolverPolicy, Listener, Protocol, TlsProfile,
    TlsVersion,
};
use tempfile::tempdir;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    time::{Duration, timeout},
};

use crate::{config_support, fixture_support, process_support, support};

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn downstream_tls_h1_forwards_absolute_form_and_connect_on_a_real_listener() {
    let origin = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
    let origin_address = origin.local_addr().expect("origin address");
    let origin_task = tokio::spawn(async move {
        let (mut request, _) = origin.accept().await.expect("absolute-form origin accept");
        let mut request_bytes = Vec::new();
        let mut buffer = [0; 1024];
        while !request_bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = request
                .read(&mut buffer)
                .await
                .expect("absolute-form request");
            assert_ne!(read, 0, "absolute-form request ended before headers");
            request_bytes.extend_from_slice(&buffer[..read]);
        }
        assert!(request_bytes.starts_with(b"GET /absolute HTTP/1.1\r\n"));
        request
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
            .await
            .expect("absolute-form origin response");

        let (mut tunnel, _) = origin.accept().await.expect("CONNECT origin accept");
        let mut payload = [0; 4];
        tunnel
            .read_exact(&mut payload)
            .await
            .expect("CONNECT tunnel payload");
        assert_eq!(&payload, b"ping");
        tunnel
            .write_all(b"pong")
            .await
            .expect("CONNECT tunnel response");
    });

    let key = fixture_support::private_key_fixture("proxy-a-key.pem");
    let token_directory = tempdir().expect("forward TLS token directory");
    let token_path = fixture_support::write_file_with_mode(
        token_directory.path(),
        "proxy.token",
        b"wire-token-012345678901234567890123\n",
        0o600,
    );
    let proxy_address = process_support::reserve_tcp_address();
    let mut config = config_support::empty_config();
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
        min_version: TlsVersion::Tls12,
        alpn: vec![AlpnProtocol::Http11],
        policy: oxiroute_config::TlsPolicy::default(),
    });
    config.forward_proxy_services.push(ForwardProxyService {
        drain_deadline_ms: None,
        name: "forward".into(),
        enabled_versions: vec![ForwardHttpVersion::H1],
        allow_absolute_form: true,
        tls_required: true,
        connect: ForwardConnectPolicy {
            enabled: true,
            allowed_ports: vec![origin_address.port()],
        },
        connect_udp: ForwardConnectPolicy::default(),
        peer_policy: ForwardPeerPolicy::default(),
        auth: Some(ForwardProxyAuth::BearerTokenFile {
            token_file_path: token_path,
        }),
        access_policy: None,
        destination_policy: ForwardDestinationPolicy {
            deny_private: false,
            ..ForwardDestinationPolicy::default()
        },
        header_policy: ForwardHeaderPolicy::default(),
        connect_timeout_ms: 1_000,
        idle_timeout_ms: 100,
        lifetime_timeout_ms: 100,
        max_request_body_bytes: Some(64 * 1024),
        max_header_bytes: 8_192,
        max_connections: 1,
        resolver: ForwardResolverPolicy::default(),
        audit_mode: ForwardAuditMode::Off,
    });
    config.listeners.push(Listener {
        name: "forward".into(),
        bind: config_support::socket_bind(proxy_address),
        protocol: Protocol::ForwardHttp1,
        service: Some("forward".into()),
        tls_profile: Some("downstream".into()),
        proxy_protocol: None,
        max_connections: Some(1),
        downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
        h2_initial_stream_window: None,
        h2_initial_connection_window: None,
    });
    let config = config.validate().expect("valid forward proxy config");
    let mut server = process_support::ServerProcess::start(&config, None);
    server.wait_for_tcp(proxy_address).await;

    let stalled = TcpStream::connect(proxy_address)
        .await
        .expect("stalled TLS connection");
    tokio::time::sleep(Duration::from_millis(250)).await;

    let mut invalid = TcpStream::connect(proxy_address)
        .await
        .expect("invalid TLS client connection");
    invalid
        .write_all(b"GET /invalid HTTP/1.1\r\nHost: proxy.example.test\r\n\r\n")
        .await
        .expect("invalid TLS client data");
    let mut invalid_response = Vec::new();
    timeout(
        Duration::from_secs(2),
        invalid.read_to_end(&mut invalid_response),
    )
    .await
    .expect("invalid TLS client close")
    .expect("invalid TLS client read");
    assert!(
        !invalid_response
            .windows(b"HTTP/".len())
            .any(|window| window == b"HTTP/")
    );

    assert_tls_alpn_rejected(proxy_address, &[b"h2"]).await;
    assert_tls_alpn_rejected(proxy_address, &[]).await;

    let mut absolute = support::tls_connect(
        proxy_address,
        support::PROXY_SERVER_NAME,
        "ca-a.pem",
        &[b"http/1.1"],
    )
    .await
    .expect("downstream H1 TLS connection");
    assert_eq!(
        support::negotiated_alpn(&absolute),
        Some(b"http/1.1".as_slice())
    );
    absolute
        .write_all(
            format!(
                "GET http://{origin_address}/absolute HTTP/1.1\r\nHost: stale\r\nProxy-Authorization: Bearer wire-token-012345678901234567890123\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .expect("absolute-form request");
    let mut absolute_response = Vec::new();
    absolute
        .read_to_end(&mut absolute_response)
        .await
        .expect("absolute-form response");
    assert!(absolute_response.starts_with(b"HTTP/1.1 200"));
    assert!(absolute_response.ends_with(b"ok"));

    let mut connect = support::tls_connect(
        proxy_address,
        support::PROXY_SERVER_NAME,
        "ca-a.pem",
        &[b"http/1.1"],
    )
    .await
    .expect("downstream CONNECT TLS connection");
    assert_eq!(
        support::negotiated_alpn(&connect),
        Some(b"http/1.1".as_slice())
    );
    connect
        .write_all(
            format!(
                "CONNECT {origin_address} HTTP/1.1\r\nHost: {origin_address}\r\nProxy-Authorization: Bearer wire-token-012345678901234567890123\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .expect("CONNECT request");
    let mut connect_head = Vec::new();
    while !connect_head.windows(4).any(|window| window == b"\r\n\r\n") {
        connect_head.push(connect.read_u8().await.expect("CONNECT response byte"));
    }
    assert!(connect_head.starts_with(b"HTTP/1.1 200"));
    connect.write_all(b"ping").await.expect("CONNECT payload");
    let mut pong = [0; 4];
    connect
        .read_exact(&mut pong)
        .await
        .expect("CONNECT response");
    assert_eq!(&pong, b"pong");

    drop(stalled);
    drop(connect);
    server.shutdown();
    origin_task.await.expect("origin task");
}

async fn assert_tls_alpn_rejected(address: std::net::SocketAddr, alpn: &[&[u8]]) {
    let config = support::tls_client_config(&support::fixture("ca-a.pem"), alpn)
        .expect("incompatible ALPN client config");
    if let Ok(mut stream) =
        support::tls_connect_with_config(address, support::PROXY_SERVER_NAME, config).await
    {
        let mut response = Vec::new();
        timeout(Duration::from_secs(30), stream.read_to_end(&mut response))
            .await
            .expect("incompatible ALPN client close")
            .expect("incompatible ALPN client read");
        assert!(
            !response
                .windows(b"HTTP/".len())
                .any(|window| window == b"HTTP/")
        );
    }
}
