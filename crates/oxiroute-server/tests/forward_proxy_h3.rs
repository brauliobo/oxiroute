#![allow(dead_code, unused_imports, clippy::duplicate_mod)]

#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/process.rs"]
mod process_support;
#[path = "support/mod.rs"]
mod support;

use std::{fs, io::Cursor, net::Ipv4Addr, sync::Arc, time::Duration};

use bytes::{Buf as _, Bytes};
use h3::client::RequestStream;
use http::{Method, Request, StatusCode};
use oxiroute_config::{
    AlpnProtocol, Certificate, CertificateSource, DownstreamTimeoutPolicy, ForwardAuditMode,
    ForwardConnectPolicy, ForwardDestinationPolicy, ForwardHeaderPolicy, ForwardHttpVersion,
    ForwardPeerPolicy, ForwardProxyService, ForwardResolverPolicy, Listener, ListenerBind,
    Protocol, TlsProfile, TlsVersion,
};
use quinn::crypto::rustls::QuicClientConfig;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpListener,
    time::{sleep, timeout},
};

const H3_ALPN: &[u8] = b"h3";

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn daemon_rejects_h3_plain_http_without_fallback_and_releases_udp_listener() {
    let origin = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("origin bind");
    let origin_address = origin.local_addr().expect("origin address");
    let mut origin_task = tokio::spawn(async move {
        let _ = origin
            .accept()
            .await
            .expect("origin accept must not receive an H3 fallback");
        panic!("HTTP/3 must not fall back to a TCP origin");
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
        connect: ForwardConnectPolicy::default(),
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
    let endpoint = client_endpoint().expect("H3 client endpoint");
    let connection = timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(connecting) = endpoint.connect(proxy_address, support::PROXY_SERVER_NAME) {
                if let Ok(connection) = connecting.await {
                    break connection;
                }
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
        .uri(format!("http://127.0.0.1:{}/h3", origin_address.port()))
        .body(())
        .expect("H3 absolute-form request");
    let mut stream = sender.send_request(request).await.expect("send H3 request");
    stream.finish().await.expect("finish H3 request");
    let response = stream.recv_response().await.expect("H3 response");
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert!(
        stream
            .recv_data()
            .await
            .expect("H3 error response")
            .is_none()
    );
    assert!(
        timeout(Duration::from_millis(500), &mut origin_task)
            .await
            .is_err(),
        "HTTP/3 request reached the TCP origin"
    );
    origin_task.abort();
    let _ = origin_task.await;

    drop(stream);
    drop(sender);
    endpoint.close(quinn::VarInt::from_u32(0), b"test complete");
    driver.await.expect("H3 driver task");
    server.shutdown_gracefully();
    let released = std::net::UdpSocket::bind(proxy_address).expect("UDP listener release");
    drop(released);
}

fn client_endpoint() -> std::io::Result<quinn::Endpoint> {
    let mut roots = rustls::RootCertStore::empty();
    let ca = fs::read(fixture_support::fixture("ca-a.pem")).expect("read H3 test CA");
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
