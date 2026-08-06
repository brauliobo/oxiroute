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
    ForwardResolverPolicy, Listener, Protocol, TlsProfile, TlsVersion, load_lua,
};
use oxiroute_import::squid::import;
use tempfile::tempdir;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    time::{Duration, timeout},
};

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
    let config = report.config.expect("finalized imported Squid candidate");
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
        .config
        .expect("finalized authenticated Squid candidate");
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
