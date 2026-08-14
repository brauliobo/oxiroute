#![allow(dead_code, unused_imports, clippy::duplicate_mod)]

#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/process.rs"]
mod process_support;
#[path = "support/mod.rs"]
mod support;

use bytes::Bytes;
use http::{Method, Request, StatusCode};
use oxiroute_config::{
    AlpnProtocol, Certificate, CertificateSource, DownstreamTimeoutPolicy, ForwardAuditMode,
    ForwardConnectPolicy, ForwardDestinationPolicy, ForwardHeaderPolicy, ForwardHttpVersion,
    ForwardPeerPolicy, ForwardProxyAuth, ForwardProxyService, ForwardResolverPolicy, Listener,
    Protocol, TlsProfile, TlsVersion,
};
use tempfile::tempdir;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpListener,
};

const TOKEN: &str = "0123456789abcdefghijklmnopqrstuv";

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn daemon_accepts_tls_h2_connect_and_relays_stream_data() {
    let origin = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
    let origin_address = origin.local_addr().expect("origin address");
    let origin_task = tokio::spawn(async move {
        let (mut stream, _) = origin.accept().await.expect("origin accept");
        let mut request = [0; 4];
        stream
            .read_exact(&mut request)
            .await
            .expect("origin CONNECT payload");
        assert_eq!(&request, b"ping");
        stream.write_all(b"pong").await.expect("origin response");
    });

    let key = fixture_support::private_key_fixture("proxy-a-key.pem");
    let token_directory = tempdir().expect("H2 token directory");
    let token_path = fixture_support::write_file_with_mode(
        token_directory.path(),
        "proxy.token",
        format!("{TOKEN}\n").as_bytes(),
        0o600,
    );
    let proxy_address = process_support::reserve_tcp_address();
    let mut config = support::empty_config();
    config.certificates.push(Certificate {
        name: "downstream".into(),
        dns_names: vec!["proxy.example.test".into()],
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
        alpn: vec![AlpnProtocol::H2],
        policy: oxiroute_config::TlsPolicy::default(),
    });
    config.forward_proxy_services.push(ForwardProxyService {
        name: "forward".into(),
        enabled_versions: vec![ForwardHttpVersion::H2],
        allow_absolute_form: false,
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
        bind: support::socket_bind(proxy_address),
        protocol: Protocol::ForwardHttp2,
        service: Some("forward".into()),
        tls_profile: Some("downstream".into()),
        proxy_protocol: None,
        max_connections: None,
        downstream_timeouts: DownstreamTimeoutPolicy::default(),
    });
    let config = config.validate().expect("valid forward H2 config");
    let mut server = process_support::ServerProcess::start(&config, None);
    server.wait_for_tcp(proxy_address).await;

    let stream = support::tls_connect(proxy_address, "proxy.example.test", "ca-a.pem", &[b"h2"])
        .await
        .expect("TLS H2 proxy connection");
    let (mut client, connection) = h2::client::handshake(stream)
        .await
        .expect("H2 client handshake");
    let driver = tokio::spawn(async move { connection.await.expect("H2 client driver") });

    let unsupported = Request::builder()
        .method(Method::GET)
        .uri(format!("http://{origin_address}/unsupported"))
        .body(())
        .expect("unsupported H2 forward request");
    let (unsupported_response, _) = client
        .send_request(unsupported, true)
        .expect("send unsupported H2 forward request");
    assert_eq!(
        unsupported_response
            .await
            .expect("receive unsupported H2 forward response")
            .status(),
        StatusCode::BAD_REQUEST
    );

    let missing_auth = Request::builder()
        .method(Method::CONNECT)
        .uri(origin_address.to_string())
        .body(())
        .expect("unauthenticated H2 CONNECT request");
    let (missing_response, _) = client
        .send_request(missing_auth, true)
        .expect("send unauthenticated H2 CONNECT");
    let missing_response = missing_response
        .await
        .expect("receive unauthenticated H2 CONNECT");
    assert_eq!(
        missing_response.status(),
        StatusCode::PROXY_AUTHENTICATION_REQUIRED
    );
    assert_eq!(missing_response.headers()["proxy-authenticate"], "Bearer");

    let request = Request::builder()
        .method(Method::CONNECT)
        .uri(origin_address.to_string())
        .header("proxy-authorization", format!("Bearer {TOKEN}"))
        .body(())
        .expect("H2 CONNECT request");
    let (response, mut request_body) = client
        .send_request(request, false)
        .expect("send H2 CONNECT");
    request_body
        .send_data(Bytes::from_static(b"ping"), true)
        .expect("send H2 tunnel payload");
    let response = response.await.expect("receive H2 CONNECT response");
    assert_eq!(response.status(), StatusCode::OK);
    let mut response_body = response.into_body();
    let payload = response_body
        .data()
        .await
        .expect("H2 tunnel response frame")
        .expect("H2 tunnel response");
    assert_eq!(&payload[..], b"pong");

    let denied_port = if origin_address.port() == u16::MAX {
        1
    } else {
        origin_address.port() + 1
    };
    let denied = Request::builder()
        .method(Method::CONNECT)
        .uri(format!("127.0.0.1:{denied_port}"))
        .header("proxy-authorization", format!("Bearer {TOKEN}"))
        .body(())
        .expect("denied H2 CONNECT request");
    let (denied_response, _) = client
        .send_request(denied, true)
        .expect("send denied H2 CONNECT");
    assert_eq!(
        denied_response
            .await
            .expect("receive denied H2 CONNECT")
            .status(),
        StatusCode::FORBIDDEN
    );

    drop(client);
    server.shutdown_gracefully();
    origin_task.await.expect("origin task");
    driver.abort();
    let _ = driver.await;
}
