use oxiroute_config::{
    ForwardAuditMode, ForwardConnectPolicy, ForwardDestinationPolicy, ForwardDirectFallback,
    ForwardHeaderPolicy, ForwardHttpVersion, ForwardPeer, ForwardPeerPolicy, ForwardProxyService,
    ForwardResolverPolicy, Listener, Protocol,
};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpListener,
};

use crate::{common::exchange, config_support, process_support};

#[tokio::test]
async fn static_peers_retry_in_order_and_receive_absolute_form() {
    let unavailable_peer = process_support::reserve_tcp_address();
    let peer = TcpListener::bind("127.0.0.1:0").await.expect("peer bind");
    let peer_address = peer.local_addr().expect("peer address");
    let peer_task = tokio::spawn(async move {
        let (mut stream, _) = peer.accept().await.expect("peer accept");
        let mut request = Vec::new();
        let mut buffer = [0; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).await.expect("peer request");
            assert_ne!(read, 0, "peer request ended before headers");
            request.extend_from_slice(&buffer[..read]);
        }
        assert!(request.starts_with(b"GET http://127.0.0.1:9/peer HTTP/1.1\r\n"));
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\npeer")
            .await
            .expect("peer response");
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
        connect_udp: ForwardConnectPolicy::default(),
        peer_policy: ForwardPeerPolicy {
            peers: vec![
                ForwardPeer {
                    host: unavailable_peer.ip().to_string(),
                    port: unavailable_peer.port(),
                },
                ForwardPeer {
                    host: peer_address.ip().to_string(),
                    port: peer_address.port(),
                },
            ],
            direct_fallback: ForwardDirectFallback::Denied,
            max_retries: 1,
        },
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

    let response = exchange(
        proxy_address,
        b"GET http://127.0.0.1:9/peer HTTP/1.1\r\nHost: target\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(response.starts_with(b"HTTP/1.1 200"));
    assert!(response.ends_with(b"peer"));

    peer_task.await.expect("peer task");
    server.shutdown();
}

#[tokio::test]
async fn failed_static_peer_can_fall_back_to_direct_http() {
    let origin = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
    let origin_address = origin.local_addr().expect("origin address");
    let origin_task = tokio::spawn(async move {
        let (mut stream, _) = origin.accept().await.expect("origin accept");
        let mut request = Vec::new();
        let mut buffer = [0; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).await.expect("origin request");
            assert_ne!(read, 0, "origin request ended before headers");
            request.extend_from_slice(&buffer[..read]);
        }
        assert!(request.starts_with(b"GET /direct HTTP/1.1\r\n"));
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\ndirect")
            .await
            .expect("origin response");
    });

    let unavailable_peer = process_support::reserve_tcp_address();
    let proxy_address = process_support::reserve_tcp_address();
    let mut config = config_support::empty_config();
    config.forward_proxy_services.push(ForwardProxyService {
        drain_deadline_ms: None,
        name: "forward".into(),
        enabled_versions: vec![ForwardHttpVersion::H1],
        allow_absolute_form: true,
        tls_required: false,
        connect: ForwardConnectPolicy::default(),
        connect_udp: ForwardConnectPolicy::default(),
        peer_policy: ForwardPeerPolicy {
            peers: vec![ForwardPeer {
                host: unavailable_peer.ip().to_string(),
                port: unavailable_peer.port(),
            }],
            direct_fallback: ForwardDirectFallback::Allowed,
            max_retries: 0,
        },
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

    let response = exchange(
        proxy_address,
        format!(
            "GET http://{origin_address}/direct HTTP/1.1\r\nHost: target\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    )
    .await;
    assert!(response.starts_with(b"HTTP/1.1 200"));
    assert!(response.ends_with(b"direct"));

    origin_task.await.expect("origin task");
    server.shutdown();
}
