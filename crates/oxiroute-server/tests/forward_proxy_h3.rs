#![allow(clippy::duplicate_mod)]

#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/process.rs"]
mod process_support;
#[path = "support/mod.rs"]
mod support;

use std::{
    fs,
    io::Cursor,
    net::Ipv4Addr,
    sync::Arc,
    time::Duration,
};

use bytes::{Buf as _, Bytes};
use h3::client::RequestStream;
use http::{Method, Request, StatusCode};
use oxiroute_config::{
    AlpnProtocol, Certificate, CertificateSource, DownstreamTimeoutPolicy, ForwardAuditMode,
    ForwardConnectPolicy, ForwardDestinationPolicy, ForwardHeaderPolicy, ForwardHttpVersion,
    ForwardProxyService, ForwardResolverPolicy, Listener, ListenerBind, Protocol, TlsProfile,
    TlsVersion,
};
use quinn::crypto::rustls::QuicClientConfig;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpListener,
    time::{sleep, timeout},
};

const H3_ALPN: &[u8] = b"h3";

#[tokio::test]
async fn daemon_accepts_active_h3_absolute_form_and_releases_udp_listener() {
    let origin = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("origin bind");
    let origin_address = origin.local_addr().expect("origin address");
    let origin_task = tokio::spawn(async move {
        let (mut stream, _) = origin.accept().await.expect("origin accept");
        let mut request = vec![0; 512];
        let bytes = stream.read(&mut request).await.expect("origin request");
        assert!(
            std::str::from_utf8(&request[..bytes])
                .expect("origin request UTF-8")
                .contains("GET /h3 HTTP/1.1")
        );
        stream
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 4\r\n\r\npong")
            .await
            .expect("origin response");
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
        max_connections: None,
        downstream_timeouts: DownstreamTimeoutPolicy::default(),
    });

    let server = process_support::ServerProcess::start(&config, None);
    let endpoint = client_endpoint().expect("H3 client endpoint");
    let connection = timeout(Duration::from_secs(10), async {
        loop {
            match endpoint.connect(proxy_address, support::PROXY_SERVER_NAME) {
                Ok(connecting) => match connecting.await {
                    Ok(connection) => break connection,
                    Err(_) => {}
                },
                Err(_) => {}
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
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(recv_chunk(&mut stream).await.as_ref(), b"pong");

    drop(stream);
    drop(sender);
    endpoint.close(quinn::VarInt::from_u32(0), b"test complete");
    driver.await.expect("H3 driver task");
    server.shutdown_gracefully();
    origin_task.await.expect("origin task");

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
