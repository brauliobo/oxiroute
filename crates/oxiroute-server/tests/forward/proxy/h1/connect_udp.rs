use oxiroute_config::{
    ForwardAuditMode, ForwardConnectPolicy, ForwardDestinationPolicy, ForwardHeaderPolicy,
    ForwardHttpVersion, ForwardPeerPolicy, ForwardProxyService, ForwardResolverPolicy, Listener,
    Protocol,
};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpStream, UdpSocket},
};

use crate::{common::exchange_head, config_support, process_support};

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn connect_udp_upgrade_relays_capsule_datagrams_on_a_real_listener() {
    let echo = UdpSocket::bind("127.0.0.1:0").await.expect("UDP echo bind");
    let echo_address = echo.local_addr().expect("UDP echo address");
    let echo_task = tokio::spawn(async move {
        let mut payload = [0; 64];
        let (length, client) = echo
            .recv_from(&mut payload)
            .await
            .expect("UDP echo receive");
        assert_eq!(&payload[..length], b"ping");
        echo.send_to(b"pong", client).await.expect("UDP echo send");
    });

    let proxy_address = process_support::reserve_tcp_address();
    let mut config = config_support::empty_config();
    config.forward_proxy_services.push(ForwardProxyService {
        drain_deadline_ms: None,
        name: "forward".into(),
        enabled_versions: vec![ForwardHttpVersion::H1],
        allow_absolute_form: true,
        tls_required: false,
        connect: ForwardConnectPolicy::default(),
        connect_udp: ForwardConnectPolicy {
            enabled: true,
            allowed_ports: vec![echo_address.port()],
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
        bind: config_support::socket_bind(proxy_address),
        protocol: Protocol::ForwardHttp1,
        service: Some("forward".into()),
        tls_profile: None,
        proxy_protocol: None,
        max_connections: None,
        downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
        h2_initial_stream_window: None,
        h2_initial_connection_window: None,
    });
    let config = config.validate().expect("valid forward proxy config");
    let mut server = process_support::ServerProcess::start(&config, None);
    server.wait_for_tcp(proxy_address).await;

    let malformed_path = exchange_head(
        proxy_address,
        format!(
            "GET http://proxy.example.test/.well-known/masque/udp/127.0.0.1/{} HTTP/1.1\r\nHost: proxy.example.test\r\nConnection: close\r\n\r\n",
            echo_address.port()
        )
        .as_bytes(),
    )
    .await;
    assert!(malformed_path.starts_with(b"HTTP/1.1 400"));

    for framing in [
        "Content-Length: 0",
        "Content-Type: application/octet-stream",
        "Transfer-Encoding: chunked",
        "Trailer: x-trailer",
    ] {
        let response = exchange_head(
            proxy_address,
            format!(
                "GET http://proxy.example.test/.well-known/masque/udp/127.0.0.1/{}/ HTTP/1.1\r\nHost: proxy.example.test\r\nConnection: Upgrade\r\nUpgrade: connect-udp\r\nCapsule-Protocol: ?1\r\n{framing}\r\n\r\n",
                echo_address.port()
            )
            .as_bytes(),
        )
        .await;
        assert!(response.starts_with(b"HTTP/1.1 400"), "accepted {framing}");
    }

    let invalid_capsule = exchange_head(
        proxy_address,
        format!(
            "GET http://proxy.example.test/.well-known/masque/udp/127.0.0.1/{}/ HTTP/1.1\r\nHost: proxy.example.test\r\nConnection: Upgrade\r\nUpgrade: connect-udp\r\nCapsule-Protocol: ?1, ?1\r\n\r\n",
            echo_address.port()
        )
        .as_bytes(),
    )
    .await;
    assert!(invalid_capsule.starts_with(b"HTTP/1.1 400"));

    let mut client = TcpStream::connect(proxy_address)
        .await
        .expect("proxy connect");
    client
        .write_all(
            format!(
                "GET http://proxy.example.test/.well-known/masque/udp/127.0.0.1/{}/ HTTP/1.1\r\nHost: proxy.example.test\r\nConnection: Upgrade\r\nUpgrade: connect-udp\r\nCapsule-Protocol: ?1;grease=\"accepted\"\r\n\r\n",
                echo_address.port()
            )
            .as_bytes(),
        )
        .await
        .expect("CONNECT-UDP request");
    let mut response = Vec::new();
    while !response.windows(4).any(|window| window == b"\r\n\r\n") {
        response.push(client.read_u8().await.expect("CONNECT-UDP response"));
    }
    let response_text = String::from_utf8_lossy(&response).to_ascii_lowercase();
    assert!(response.starts_with(b"HTTP/1.1 101"));
    assert!(response_text.contains("upgrade: connect-udp"));
    assert!(response_text.contains("capsule-protocol: ?1"));

    client
        .write_all(&[0, 5, 0, b'p', b'i', b'n', b'g'])
        .await
        .expect("UDP datagram capsule");
    let mut capsule = [0; 7];
    client
        .read_exact(&mut capsule)
        .await
        .expect("UDP response capsule");
    assert_eq!(&capsule, &[0, 5, 0, b'p', b'o', b'n', b'g']);

    drop(client);
    echo_task.await.expect("UDP echo task");
    server.shutdown();
}
