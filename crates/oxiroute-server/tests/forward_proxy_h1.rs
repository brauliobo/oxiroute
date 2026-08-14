#![allow(dead_code, unused_imports, clippy::duplicate_mod)]

#[path = "support/config.rs"]
mod config_support;
#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/process.rs"]
mod process_support;
#[path = "support/mod.rs"]
mod support;

use std::{fs, os::unix::fs::PermissionsExt as _, path::Path};

use oxiroute_config::{
    AlpnProtocol, Certificate, CertificateSource, ForwardAccessAction, ForwardAccessCondition,
    ForwardAccessMatcher, ForwardAccessPolicy, ForwardAccessRule, ForwardAuditMode,
    ForwardConnectPolicy, ForwardDestinationPolicy, ForwardDirectFallback, ForwardHeaderPolicy,
    ForwardHttpVersion, ForwardPeer, ForwardPeerPolicy, ForwardProxyAuth, ForwardProxyService,
    ForwardResolverPolicy, Listener, Protocol, Stats, TlsProfile, TlsVersion,
};
use oxiroute_import::squid::import;
use tempfile::tempdir;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream, UdpSocket},
    time::{Duration, timeout},
};

use config_support::load_lua;

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
        timeout(Duration::from_secs(2), stream.read_to_end(&mut response))
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

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn basic_authenticated_absolute_form_and_connect_cross_the_runtime_listener() {
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
        assert!(request.starts_with(b"GET /through HTTP/1.1\r\n"));
        assert!(
            !request
                .windows(b"proxy-authorization".len())
                .any(|window| window.eq_ignore_ascii_case(b"proxy-authorization"))
        );
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
            .await
            .expect("origin response");
        let (mut anonymous, _) = origin.accept().await.expect("anonymous origin accept");
        let mut request = Vec::new();
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = anonymous
                .read(&mut buffer)
                .await
                .expect("anonymous origin read");
            assert_ne!(read, 0, "anonymous request ended before headers");
            request.extend_from_slice(&buffer[..read]);
        }
        assert!(request.starts_with(b"OPTIONS /anonymous HTTP/1.1\r\n"));
        anonymous
            .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await
            .expect("anonymous origin response");
        let (mut early, _) = origin.accept().await.expect("early origin accept");
        let mut request = Vec::new();
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = early.read(&mut buffer).await.expect("early origin read");
            assert_ne!(read, 0, "early request ended before headers");
            request.extend_from_slice(&buffer[..read]);
        }
        early
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
            .await
            .expect("early origin response");
        let (mut limited, _) = origin.accept().await.expect("limited body origin accept");
        let mut request = Vec::new();
        loop {
            match limited.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(read) => request.extend_from_slice(&buffer[..read]),
            }
        }
        assert!(request.starts_with(b"POST /limited HTTP/1.1\r\n"));
        let (mut cached, _) = origin
            .accept()
            .await
            .expect("cached credential origin accept");
        let mut request = Vec::new();
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = cached
                .read(&mut buffer)
                .await
                .expect("cached credential origin read");
            assert_ne!(read, 0, "cached credential request ended before headers");
            request.extend_from_slice(&buffer[..read]);
        }
        cached
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
            .await
            .expect("cached credential origin response");
        let (mut stalled, _) = origin.accept().await.expect("stalled origin accept");
        let mut request = Vec::new();
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stalled
                .read(&mut buffer)
                .await
                .expect("stalled origin read");
            assert_ne!(read, 0, "stalled request ended before headers");
            request.extend_from_slice(&buffer[..read]);
        }
        stalled
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nConnection: close\r\n\r\n")
            .await
            .expect("stalled response headers");
        tokio::time::sleep(Duration::from_millis(1_500)).await;
        let (mut tunnel, _) = origin.accept().await.expect("tunnel accept");
        let mut payload = [0; 4];
        tunnel
            .read_exact(&mut payload)
            .await
            .expect("tunnel payload");
        assert_eq!(&payload, b"ping");
        tunnel.write_all(b"pong").await.expect("tunnel response");
        let mut closed = [0];
        assert_eq!(tunnel.read(&mut closed).await.expect("tunnel shutdown"), 0);
    });

    let directory = tempdir().expect("auth directory");
    let htpasswd = directory.path().join("proxy.htpasswd");
    fs::write(&htpasswd, "myname:$apr1$r31.....$HqJZimcKQFAMYayBlzkrA/\n").expect("write htpasswd");
    fs::set_permissions(&htpasswd, fs::Permissions::from_mode(0o600)).expect("htpasswd mode");
    let proxy_address = process_support::reserve_tcp_address();
    let mut config = config_support::empty_config();
    config.max_connections = None;
    config.forward_proxy_services.push(ForwardProxyService {
        name: "forward".into(),
        enabled_versions: vec![ForwardHttpVersion::H1],
        allow_absolute_form: true,
        tls_required: false,
        connect: ForwardConnectPolicy {
            enabled: true,
            allowed_ports: vec![origin_address.port()],
        },
        connect_udp: ForwardConnectPolicy::default(),
        peer_policy: ForwardPeerPolicy::default(),
        auth: Some(ForwardProxyAuth::BasicHtpasswdFile {
            htpasswd_file_path: htpasswd.clone(),
            realm: "Private proxy".into(),
            credential_ttl_ms: Some(50),
            username_case_sensitive: false,
        }),
        access_policy: Some(ForwardAccessPolicy {
            rules: vec![
                ForwardAccessRule {
                    action: ForwardAccessAction::Allow,
                    conditions: vec![
                        ForwardAccessCondition {
                            negated: false,
                            matcher: ForwardAccessMatcher::Methods {
                                methods: vec!["OPTIONS".into()],
                            },
                        },
                        ForwardAccessCondition {
                            negated: true,
                            matcher: ForwardAccessMatcher::Authenticated,
                        },
                    ],
                },
                ForwardAccessRule {
                    action: ForwardAccessAction::Allow,
                    conditions: vec![ForwardAccessCondition {
                        negated: false,
                        matcher: ForwardAccessMatcher::Authenticated,
                    }],
                },
                ForwardAccessRule {
                    action: ForwardAccessAction::Deny,
                    conditions: vec![ForwardAccessCondition {
                        negated: false,
                        matcher: ForwardAccessMatcher::All,
                    }],
                },
            ],
            default_action: ForwardAccessAction::Deny,
        }),
        destination_policy: ForwardDestinationPolicy {
            deny_private: false,
            ..ForwardDestinationPolicy::default()
        },
        header_policy: ForwardHeaderPolicy::default(),
        connect_timeout_ms: 1_000,
        idle_timeout_ms: 1_000,
        lifetime_timeout_ms: 5_000,
        max_request_body_bytes: Some(1024),
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
        tls_profile: None,
        proxy_protocol: None,
        max_connections: None,
        downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
    });
    let config = config.validate().expect("valid forward proxy config");
    let mut server = process_support::ServerProcess::start(&config, None);
    server.wait_for_tcp(proxy_address).await;

    let mut slow = TcpStream::connect(proxy_address)
        .await
        .expect("slow proxy connection");
    slow.write_all(b"GET http://127.0.0.1/ HTTP/1.1\r\n")
        .await
        .expect("partial proxy header");
    let mut byte = [0];
    let slow_read = timeout(Duration::from_secs(2), slow.read(&mut byte))
        .await
        .expect("request header timeout");
    assert!(matches!(slow_read, Ok(0) | Err(_)));
    drop(slow);
    tokio::time::sleep(Duration::from_millis(20)).await;

    let missing = exchange(
        proxy_address,
        format!(
            "GET http://{origin_address}/through HTTP/1.1\r\nHost: stale\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    )
    .await;
    assert!(missing.starts_with(b"HTTP/1.1 407"));
    assert!(
        String::from_utf8_lossy(&missing)
            .to_ascii_lowercase()
            .contains("proxy-authenticate: basic realm=\"private proxy\"")
    );
    let unresolved_unauthenticated = exchange(
        proxy_address,
        b"GET http://does-not-exist.invalid/ HTTP/1.1\r\nHost: stale\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(unresolved_unauthenticated.starts_with(b"HTTP/1.1 407"));

    let oversized = exchange(
        proxy_address,
        format!(
            "POST http://{origin_address}/through HTTP/1.1\r\nHost: stale\r\nProxy-Authorization: Basic bXlOYW1lOm15UGFzc3dvcmQ=\r\nContent-Length: 2048\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    )
    .await;
    assert!(oversized.starts_with(b"HTTP/1.1 413"));

    let accepted = exchange(
        proxy_address,
        format!(
            "GET http://{origin_address}/through HTTP/1.1\r\nHost: stale\r\nProxy-Authorization: Basic TVlOQU1FOm15UGFzc3dvcmQ=\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    )
    .await;
    assert!(accepted.starts_with(b"HTTP/1.1 200"));
    assert!(accepted.ends_with(b"ok"));

    let anonymous = exchange(
        proxy_address,
        format!(
            "OPTIONS http://{origin_address}/anonymous HTTP/1.1\r\nHost: stale\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    )
    .await;
    assert!(anonymous.starts_with(b"HTTP/1.1 204"));

    let chunk = "x".repeat(1_025);
    let mut chunked = TcpStream::connect(proxy_address)
        .await
        .expect("chunked proxy connection");
    chunked
        .write_all(
            format!(
                "POST http://{origin_address}/early HTTP/1.1\r\nHost: stale\r\nProxy-Authorization: Basic bXlOYW1lOm15UGFzc3dvcmQ=\r\nTransfer-Encoding: chunked\r\nExpect: 100-continue\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .expect("chunked request headers");
    let mut interim = Vec::new();
    while !interim.windows(4).any(|window| window == b"\r\n\r\n") {
        interim.push(chunked.read_u8().await.expect("interim response byte"));
    }
    assert!(interim.starts_with(b"HTTP/1.1 100"));
    let mut early_response = Vec::new();
    chunked
        .read_to_end(&mut early_response)
        .await
        .expect("early origin response");
    assert!(early_response.starts_with(b"HTTP/1.1 200"));

    let eager_chunked_oversized = exchange(
        proxy_address,
        format!(
            "POST http://{origin_address}/limited HTTP/1.1\r\nHost: stale\r\nProxy-Authorization: Basic bXlOYW1lOm15UGFzc3dvcmQ=\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n401\r\n{chunk}\r\n0\r\n\r\n"
        )
        .as_bytes(),
    )
    .await;
    assert!(eager_chunked_oversized.starts_with(b"HTTP/1.1 413"));

    fs::write(&htpasswd, "invalid\n").expect("invalidate htpasswd");
    tokio::time::sleep(Duration::from_millis(5)).await;
    let cached = exchange(
        proxy_address,
        format!(
            "GET http://{origin_address}/through HTTP/1.1\r\nHost: stale\r\nProxy-Authorization: Basic TVlOQU1FOm15UGFzc3dvcmQ=\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    )
    .await;
    assert!(cached.starts_with(b"HTTP/1.1 200"));
    tokio::time::sleep(Duration::from_millis(60)).await;
    let expired = exchange(
        proxy_address,
        format!(
            "GET http://{origin_address}/through HTTP/1.1\r\nHost: stale\r\nProxy-Authorization: Basic bXlOYW1lOm15UGFzc3dvcmQ=\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    )
    .await;
    assert!(expired.starts_with(b"HTTP/1.1 407"));
    fs::write(&htpasswd, "myname:$apr1$r31.....$HqJZimcKQFAMYayBlzkrA/\n")
        .expect("restore htpasswd");
    tokio::time::sleep(Duration::from_millis(5)).await;

    let stalled = exchange_partial(
        proxy_address,
        format!(
            "GET http://{origin_address}/stalled HTTP/1.1\r\nHost: stale\r\nProxy-Authorization: Basic bXlOYW1lOm15UGFzc3dvcmQ=\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    )
    .await;
    assert!(stalled.starts_with(b"HTTP/1.1 200"));
    assert!(!stalled.ends_with(b"x"));

    let mut tunnel = TcpStream::connect(proxy_address)
        .await
        .expect("CONNECT proxy");
    tunnel
        .write_all(
            format!(
                "CONNECT {origin_address} HTTP/1.1\r\nHost: {origin_address}\r\nProxy-Authorization: Basic bXlOYW1lOm15UGFzc3dvcmQ=\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .expect("CONNECT request");
    let mut response_head = Vec::new();
    while !response_head.windows(4).any(|window| window == b"\r\n\r\n") {
        response_head.push(tunnel.read_u8().await.expect("CONNECT response byte"));
    }
    assert!(response_head.starts_with(b"HTTP/1.1 200"));
    let mut excess = TcpStream::connect(proxy_address)
        .await
        .expect("excess proxy connection");
    let _ = excess
        .write_all(b"GET http://127.0.0.1/ HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
        .await;
    let mut byte = [0];
    let excess_read = timeout(Duration::from_secs(1), excess.read(&mut byte))
        .await
        .expect("excess connection close timeout");
    assert!(matches!(excess_read, Ok(0) | Err(_)));
    tunnel
        .write_all(b"ping")
        .await
        .expect("post-200 tunnel payload");
    let mut pong = [0; 4];
    timeout(Duration::from_secs(5), tunnel.read_exact(&mut pong))
        .await
        .expect("tunnel response timeout")
        .expect("tunnel response");
    assert_eq!(&pong, b"pong");
    server.shutdown_gracefully();
    origin_task.await.expect("origin task");
}

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

#[tokio::test]
async fn cache_serves_a_second_absolute_form_get_without_reaching_the_origin() {
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
        assert!(request.starts_with(b"GET /cached HTTP/1.1\r\n"));
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nCache-Control: max-age=60\r\nContent-Length: 6\r\nConnection: close\r\n\r\ncached",
            )
            .await
            .expect("origin response");
    });

    let proxy_address = process_support::reserve_tcp_address();
    let config = load_lua(&format!(
        r#"return {{
  version = 1,
  listeners = {{ {{
    name = "forward",
    bind = {{ type = "socket", address = "{proxy_address}" }},
    protocol = "forward_http1",
    service = "forward",
  }} }},
  cache_stores = {{ {{ name = "memory", type = "memory" }} }},
  forward_proxy_services = {{ {{
    name = "forward",
    tls_required = false,
    destination_policy = {{ deny_private = false }},
    cache = {{ store = "memory" }},
  }} }},
}}"#,
    ))
    .expect("forward cache config");
    let mut server = process_support::ServerProcess::start(&config, None);
    server.wait_for_tcp(proxy_address).await;

    let first = exchange(
        proxy_address,
        format!(
            "GET http://{origin_address}/cached HTTP/1.1\r\nHost: origin\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    )
    .await;
    assert!(first.starts_with(b"HTTP/1.1 200"));
    assert!(first.ends_with(b"cached"));

    tokio::time::sleep(Duration::from_millis(20)).await;
    let second = exchange(
        proxy_address,
        format!(
            "GET http://{origin_address}/cached HTTP/1.1\r\nHost: origin\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    )
    .await;
    assert!(second.starts_with(b"HTTP/1.1 200"));
    assert!(second.ends_with(b"cached"));

    origin_task.await.expect("origin task");
    server.shutdown();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn authenticated_absolute_form_bypasses_anonymous_forward_cache() {
    let origin = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
    let origin_address = origin.local_addr().expect("origin address");
    let origin_task = tokio::spawn(async move {
        let mut buffer = [0; 1024];
        let (mut anonymous, _) = origin.accept().await.expect("anonymous origin accept");
        let mut request = Vec::new();
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = anonymous
                .read(&mut buffer)
                .await
                .expect("anonymous origin read");
            assert_ne!(read, 0, "anonymous request ended before headers");
            request.extend_from_slice(&buffer[..read]);
        }
        assert!(request.starts_with(b"GET /boundary HTTP/1.1\r\n"));
        anonymous
            .write_all(
                b"HTTP/1.1 200 OK\r\nCache-Control: max-age=60\r\nContent-Length: 6\r\nConnection: close\r\n\r\ncached",
            )
            .await
            .expect("anonymous origin response");

        let (mut authenticated, _) = origin.accept().await.expect("authenticated origin accept");
        let mut request = Vec::new();
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = authenticated
                .read(&mut buffer)
                .await
                .expect("authenticated origin read");
            assert_ne!(read, 0, "authenticated request ended before headers");
            request.extend_from_slice(&buffer[..read]);
        }
        assert!(request.starts_with(b"GET /boundary HTTP/1.1\r\n"));
        authenticated
            .write_all(
                b"HTTP/1.1 200 OK\r\nCache-Control: max-age=60\r\nContent-Length: 7\r\nConnection: close\r\n\r\nprivate",
            )
            .await
            .expect("authenticated origin response");
    });

    let token_directory = tempdir().expect("forward token directory");
    let token = "0123456789abcdefghijklmnopqrstuv";
    let token_path = fixture_support::write_file_with_mode(
        token_directory.path(),
        "proxy.token",
        format!("{token}\n").as_bytes(),
        0o600,
    );
    let proxy_address = process_support::reserve_tcp_address();
    let config = load_lua(&format!(
        r#"return {{
  version = 1,
  listeners = {{ {{
    name = "forward",
    bind = {{ type = "socket", address = "{proxy_address}" }},
    protocol = "forward_http1",
    service = "forward",
  }} }},
  cache_stores = {{ {{ name = "memory", type = "memory" }} }},
  forward_proxy_services = {{ {{
    name = "forward",
    tls_required = false,
    auth = {{ type = "bearer_token_file", token_file_path = "{}" }},
    access_policy = {{
      rules = {{
        {{ action = "allow", conditions = {{
          {{ type = "methods", methods = {{ "GET" }} }},
          {{ type = "authenticated", negated = true }},
        }} }},
        {{ action = "allow", conditions = {{ {{ type = "authenticated" }} }} }},
      }},
      default_action = "deny",
    }},
    destination_policy = {{ deny_private = false }},
    cache = {{ store = "memory" }},
  }} }},
}}"#,
        token_path.display()
    ))
    .expect("forward cache boundary config");
    let mut server = process_support::ServerProcess::start(&config, None);
    server.wait_for_tcp(proxy_address).await;

    let anonymous = exchange(
        proxy_address,
        format!(
            "GET http://{origin_address}/boundary HTTP/1.1\r\nHost: origin\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    )
    .await;
    assert!(anonymous.starts_with(b"HTTP/1.1 200"));
    assert!(anonymous.ends_with(b"cached"));

    tokio::time::sleep(Duration::from_millis(20)).await;
    let authenticated = exchange(
        proxy_address,
        format!(
            "GET http://{origin_address}/boundary HTTP/1.1\r\nHost: origin\r\nProxy-Authorization: Bearer {token}\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    )
    .await;
    assert!(authenticated.starts_with(b"HTTP/1.1 200"));
    assert!(authenticated.ends_with(b"private"));

    let cached = exchange(
        proxy_address,
        format!(
            "GET http://{origin_address}/boundary HTTP/1.1\r\nHost: origin\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    )
    .await;
    assert!(cached.starts_with(b"HTTP/1.1 200"));
    assert!(cached.ends_with(b"cached"));

    timeout(Duration::from_secs(2), origin_task)
        .await
        .expect("origin boundary task timeout")
        .expect("origin boundary task");
    server.shutdown();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn cache_collapses_gets_preserves_head_only_if_cached_and_exact_metrics() {
    let origin = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
    let origin_address = origin.local_addr().expect("origin address");
    let origin_task = tokio::spawn(async move {
        let mut buffer = [0; 1024];
        let (mut leader, _) = origin.accept().await.expect("collapsed leader accept");
        let request = read_request_head(&mut leader, &mut buffer).await;
        assert!(request.starts_with(b"GET /collapse HTTP/1.1\r\n"));
        tokio::time::sleep(Duration::from_millis(100)).await;
        leader
            .write_all(
                b"HTTP/1.1 200 OK\r\nCache-Control: max-age=60\r\nContent-Length: 9\r\nConnection: close\r\n\r\ncollapsed",
            )
            .await
            .expect("collapsed response");

        for _ in 0..2 {
            let (mut head, _) = origin.accept().await.expect("HEAD origin accept");
            let request = read_request_head(&mut head, &mut buffer).await;
            assert!(request.starts_with(b"HEAD /head HTTP/1.1\r\n"));
            head.write_all(
                b"HTTP/1.1 200 OK\r\nCache-Control: max-age=60\r\nContent-Length: 4\r\nConnection: close\r\n\r\n",
            )
            .await
            .expect("HEAD origin response");
        }
    });

    let proxy_address = process_support::reserve_tcp_address();
    let stats_address = process_support::reserve_tcp_address();
    let mut config = forward_cache_config(proxy_address);
    config.stats = Some(Stats {
        binds: vec![stats_address],
        admin_token_file: None,
        pages: Vec::new(),
    });
    let config = config.validate().expect("valid forward proxy config");
    let mut server = process_support::ServerProcess::start(&config, None);
    server.wait_for_tcp(proxy_address).await;
    server.wait_for_tcp(stats_address).await;

    let request = format!(
        "GET http://{origin_address}/collapse HTTP/1.1\r\nHost: origin\r\nConnection: close\r\n\r\n"
    );
    let leader_request = request.clone().into_bytes();
    let leader = tokio::spawn(async move { exchange(proxy_address, &leader_request).await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let follower_request = request.into_bytes();
    let follower = tokio::spawn(async move { exchange(proxy_address, &follower_request).await });
    let leader = leader.await.expect("collapsed leader task");
    let follower = follower.await.expect("collapsed follower task");
    assert!(leader.starts_with(b"HTTP/1.1 200"));
    assert!(follower.starts_with(b"HTTP/1.1 200"));
    assert!(leader.ends_with(b"collapsed"));
    assert!(follower.ends_with(b"collapsed"));

    let only_if_cached = exchange(
        proxy_address,
        format!(
            "GET http://{origin_address}/missing HTTP/1.1\r\nHost: origin\r\nCache-Control: only-if-cached\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    )
    .await;
    assert!(only_if_cached.starts_with(b"HTTP/1.1 504"));

    for _ in 0..2 {
        let head = exchange(
            proxy_address,
            format!(
                "HEAD http://{origin_address}/head HTTP/1.1\r\nHost: origin\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .await;
        assert!(head.starts_with(b"HTTP/1.1 200"));
        assert!(head.ends_with(b"\r\n\r\n"));
    }

    tokio::time::sleep(Duration::from_millis(20)).await;
    let metrics = exchange(
        stats_address,
        b"GET /metrics HTTP/1.1\r\nHost: stats\r\nConnection: close\r\n\r\n",
    )
    .await;
    let metrics = String::from_utf8(metrics).expect("metrics UTF-8");
    assert!(metrics.contains("oxiroute_http_cache_hits_total{listener=\"forward\"} 1"));
    assert!(metrics.contains("oxiroute_http_cache_misses_total{listener=\"forward\"} 5"));
    assert!(metrics.contains("oxiroute_http_cache_admissions_total{listener=\"forward\"} 1"));
    assert!(metrics.contains("oxiroute_http_cache_evictions_total{listener=\"forward\"} 0"));

    origin_task.await.expect("cache lifecycle origin task");
    server.shutdown();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn cache_revalidates_304_and_serves_stale_only_for_upstream_failures() {
    let origin = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
    let origin_address = origin.local_addr().expect("origin address");
    let origin_task = tokio::spawn(async move {
        let mut buffer = [0; 1024];
        let (mut initial, _) = origin.accept().await.expect("initial origin accept");
        let request = read_request_head(&mut initial, &mut buffer).await;
        assert!(request.starts_with(b"GET /stale HTTP/1.1\r\n"));
        initial
            .write_all(
                b"HTTP/1.1 200 OK\r\nCache-Control: max-age=0, stale-if-error=60\r\nETag: \"v1\"\r\nContent-Length: 5\r\nConnection: close\r\n\r\nstale",
            )
            .await
            .expect("initial stale response");

        let (mut revalidation, _) = origin.accept().await.expect("304 origin accept");
        let request = read_request_head(&mut revalidation, &mut buffer).await;
        assert!(request.starts_with(b"GET /stale HTTP/1.1\r\n"));
        assert!(
            request
                .windows(19)
                .any(|window| window.eq_ignore_ascii_case(b"if-none-match: \"v1\""))
        );
        revalidation
            .write_all(
                b"HTTP/1.1 304 Not Modified\r\nCache-Control: max-age=0, stale-if-error=60\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await
            .expect("304 response");

        let (mut local, _) = origin.accept().await.expect("local failure origin accept");
        assert_eq!(
            local.read(&mut buffer).await.expect("local failure close"),
            0,
            "locally rejected request reached the origin"
        );

        let (mut failed, _) = origin.accept().await.expect("failed origin accept");
        let request = read_request_head(&mut failed, &mut buffer).await;
        assert!(request.starts_with(b"GET /stale HTTP/1.1\r\n"));
        failed
            .write_all(
                b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await
            .expect("upstream server failure");
    });

    let proxy_address = process_support::reserve_tcp_address();
    let stats_address = process_support::reserve_tcp_address();
    let mut config = forward_cache_config(proxy_address);
    config.stats = Some(Stats {
        binds: vec![stats_address],
        admin_token_file: None,
        pages: Vec::new(),
    });
    let config = config.validate().expect("valid forward proxy config");
    let mut server = process_support::ServerProcess::start(&config, None);
    server.wait_for_tcp(proxy_address).await;
    server.wait_for_tcp(stats_address).await;
    let normal = format!(
        "GET http://{origin_address}/stale HTTP/1.1\r\nHost: origin\r\nConnection: close\r\n\r\n"
    );

    let initial = exchange(proxy_address, normal.as_bytes()).await;
    assert!(initial.starts_with(b"HTTP/1.1 200"));
    assert!(initial.ends_with(b"stale"));
    let revalidated = exchange(proxy_address, normal.as_bytes()).await;
    assert!(revalidated.starts_with(b"HTTP/1.1 200"));
    assert!(revalidated.ends_with(b"stale"));

    let local_failure = exchange_head(
        proxy_address,
        format!(
            "GET http://{origin_address}/stale HTTP/1.1\r\nHost: origin\r\nConnection: bad name\r\n\r\n"
        )
        .as_bytes(),
    )
    .await;
    assert!(local_failure.starts_with(b"HTTP/1.1 400"));

    let stale = exchange(proxy_address, normal.as_bytes()).await;
    assert!(stale.starts_with(b"HTTP/1.1 200"));
    assert!(stale.ends_with(b"stale"));

    let metrics = exchange(
        stats_address,
        b"GET /metrics HTTP/1.1\r\nHost: stats\r\nConnection: close\r\n\r\n",
    )
    .await;
    let metrics = String::from_utf8(metrics).expect("metrics UTF-8");
    assert!(metrics.contains("oxiroute_http_cache_hits_total{listener=\"forward\"} 1"));
    assert!(metrics.contains("oxiroute_http_cache_misses_total{listener=\"forward\"} 4"));
    assert!(metrics.contains("oxiroute_http_cache_admissions_total{listener=\"forward\"} 1"));

    origin_task.await.expect("revalidation origin task");
    server.shutdown();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn cache_cancels_disconnected_leaders_but_not_leaders_with_cancelled_followers() {
    let origin = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
    let origin_address = origin.local_addr().expect("origin address");
    let origin_task = tokio::spawn(async move {
        let mut buffer = [0; 1024];
        let (mut partial, _) = origin.accept().await.expect("partial origin accept");
        let request = read_request_head(&mut partial, &mut buffer).await;
        assert!(request.starts_with(b"GET /partial HTTP/1.1\r\n"));
        partial
            .write_all(
                b"HTTP/1.1 200 OK\r\nCache-Control: max-age=60\r\nContent-Length: 6\r\nConnection: close\r\n\r\nabc",
            )
            .await
            .expect("partial origin response");
        tokio::time::sleep(Duration::from_millis(150)).await;
        let _ = partial.write_all(b"def").await;

        let (mut retry, _) = origin.accept().await.expect("partial retry accept");
        let request = read_request_head(&mut retry, &mut buffer).await;
        assert!(request.starts_with(b"GET /partial HTTP/1.1\r\n"));
        retry
            .write_all(
                b"HTTP/1.1 200 OK\r\nCache-Control: max-age=60\r\nContent-Length: 5\r\nConnection: close\r\n\r\nfresh",
            )
            .await
            .expect("partial retry response");

        let (mut leader, _) = origin.accept().await.expect("follower leader accept");
        let request = read_request_head(&mut leader, &mut buffer).await;
        assert!(request.starts_with(b"GET /follower HTTP/1.1\r\n"));
        tokio::time::sleep(Duration::from_millis(150)).await;
        leader
            .write_all(
                b"HTTP/1.1 200 OK\r\nCache-Control: max-age=60\r\nContent-Length: 6\r\nConnection: close\r\n\r\nleader",
            )
            .await
            .expect("follower leader response");
    });

    let proxy_address = process_support::reserve_tcp_address();
    let config = forward_cache_config(proxy_address);
    let config = config.validate().expect("valid forward proxy config");
    let mut server = process_support::ServerProcess::start(&config, None);
    server.wait_for_tcp(proxy_address).await;

    let mut disconnected = TcpStream::connect(proxy_address)
        .await
        .expect("partial proxy connection");
    disconnected
        .write_all(
            format!(
                "GET http://{origin_address}/partial HTTP/1.1\r\nHost: origin\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .expect("partial proxy request");
    let mut partial = Vec::new();
    while !partial.ends_with(b"abc") {
        partial.push(
            timeout(Duration::from_secs(2), disconnected.read_u8())
                .await
                .expect("partial response timeout")
                .expect("partial response byte"),
        );
    }
    drop(disconnected);
    tokio::time::sleep(Duration::from_millis(200)).await;

    let retry = exchange(
        proxy_address,
        format!(
            "GET http://{origin_address}/partial HTTP/1.1\r\nHost: origin\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    )
    .await;
    assert!(retry.starts_with(b"HTTP/1.1 200"));
    assert!(retry.ends_with(b"fresh"));

    let follower_request = format!(
        "GET http://{origin_address}/follower HTTP/1.1\r\nHost: origin\r\nConnection: close\r\n\r\n"
    );
    let leader_request = follower_request.clone().into_bytes();
    let leader = tokio::spawn(async move { exchange(proxy_address, &leader_request).await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let mut follower = TcpStream::connect(proxy_address)
        .await
        .expect("follower proxy connection");
    follower
        .write_all(follower_request.as_bytes())
        .await
        .expect("follower proxy request");
    tokio::time::sleep(Duration::from_millis(30)).await;
    drop(follower);

    let leader = leader.await.expect("cache leader task");
    assert!(leader.starts_with(b"HTTP/1.1 200"));
    assert!(leader.ends_with(b"leader"));
    tokio::time::sleep(Duration::from_millis(20)).await;
    let cached = exchange(proxy_address, follower_request.as_bytes()).await;
    assert!(cached.starts_with(b"HTTP/1.1 200"));
    assert!(cached.ends_with(b"leader"));

    origin_task.await.expect("cancellation origin task");
    server.shutdown();
}

#[tokio::test]
async fn cache_admits_only_after_eos_and_rejects_trailered_framing() {
    let origin = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
    let origin_address = origin.local_addr().expect("origin address");
    let origin_task = tokio::spawn(async move {
        let mut buffer = [0; 1024];
        for body in [b"first".as_slice(), b"second".as_slice()] {
            let (mut stream, _) = origin.accept().await.expect("trailered origin accept");
            let request = read_request_head(&mut stream, &mut buffer).await;
            assert!(request.starts_with(b"GET /trailered HTTP/1.1\r\n"));
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nCache-Control: max-age=60\r\nTransfer-Encoding: chunked\r\nTrailer: x-checksum\r\nConnection: close\r\n\r\n",
                )
                .await
                .expect("trailered response headers");
            stream
                .write_all(format!("{:x}\r\n", body.len()).as_bytes())
                .await
                .expect("trailered chunk size");
            stream
                .write_all(body)
                .await
                .expect("trailered response body");
            stream
                .write_all(b"\r\n0\r\nx-checksum: present\r\n\r\n")
                .await
                .expect("trailered response end");
        }
    });

    let proxy_address = process_support::reserve_tcp_address();
    let config = forward_cache_config(proxy_address);
    let config = config.validate().expect("valid forward proxy config");
    let mut server = process_support::ServerProcess::start(&config, None);
    server.wait_for_tcp(proxy_address).await;
    let request = format!(
        "GET http://{origin_address}/trailered HTTP/1.1\r\nHost: origin\r\nConnection: close\r\n\r\n"
    );

    let first = exchange(proxy_address, request.as_bytes()).await;
    assert!(first.starts_with(b"HTTP/1.1 200"));
    assert!(first.windows(5).any(|window| window == b"first"));
    let second = exchange(proxy_address, request.as_bytes()).await;
    assert!(second.starts_with(b"HTTP/1.1 200"));
    assert!(second.windows(6).any(|window| window == b"second"));

    origin_task.await.expect("trailered origin task");
    server.shutdown();
}

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
        .join("../oxiroute-import/tests/fixtures/squid/hostrouter-sanitized.conf");
    let source = fs::read_to_string(fixture)
        .expect("read audited Squid fixture")
        .replace("http_port 31280", &format!("http_port {proxy_address}"))
        .replace(
            "/tmp/squid-synthetic-users",
            &htpasswd.display().to_string(),
        );
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

async fn exchange(address: std::net::SocketAddr, request: &[u8]) -> Vec<u8> {
    timeout(Duration::from_secs(5), async {
        let mut stream = TcpStream::connect(address).await.expect("proxy connect");
        stream.write_all(request).await.expect("proxy request");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .expect("proxy response");
        response
    })
    .await
    .expect("proxy exchange timeout")
}

fn forward_cache_config(address: std::net::SocketAddr) -> oxiroute_config::ConfigDraft {
    load_lua(&format!(
        r#"return {{
  version = 1,
  listeners = {{ {{
    name = "forward",
    bind = {{ type = "socket", address = "{address}" }},
    protocol = "forward_http1",
    service = "forward",
  }} }},
  cache_stores = {{ {{ name = "memory", type = "memory" }} }},
  forward_proxy_services = {{ {{
    name = "forward",
    tls_required = false,
    destination_policy = {{ deny_private = false }},
    cache = {{ store = "memory" }},
  }} }},
}}"#,
    ))
    .expect("forward cache config")
    .to_draft()
}

async fn read_request_head(stream: &mut TcpStream, buffer: &mut [u8]) -> Vec<u8> {
    let mut request = Vec::new();
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = stream.read(buffer).await.expect("origin request read");
        assert_ne!(read, 0, "origin request ended before headers");
        request.extend_from_slice(&buffer[..read]);
    }
    request
}

async fn exchange_head(address: std::net::SocketAddr, request: &[u8]) -> Vec<u8> {
    timeout(Duration::from_secs(5), async {
        let mut stream = TcpStream::connect(address).await.expect("proxy connect");
        stream.write_all(request).await.expect("proxy request");
        let mut response = Vec::new();
        while !response.windows(4).any(|window| window == b"\r\n\r\n") {
            response.push(stream.read_u8().await.expect("proxy response byte"));
        }
        response
    })
    .await
    .expect("proxy response head timeout")
}

async fn exchange_partial(address: std::net::SocketAddr, request: &[u8]) -> Vec<u8> {
    timeout(Duration::from_secs(5), async {
        let mut stream = TcpStream::connect(address).await.expect("proxy connect");
        stream.write_all(request).await.expect("proxy request");
        let mut response = Vec::new();
        let _ = stream.read_to_end(&mut response).await;
        response
    })
    .await
    .expect("proxy partial exchange timeout")
}
