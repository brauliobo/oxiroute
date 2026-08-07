#![cfg(unix)]
#![allow(
    dead_code,
    unused_imports,
    clippy::duplicate_mod,
    clippy::too_many_lines
)]

#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/process.rs"]
mod process_support;
#[path = "support/mod.rs"]
mod support;

use std::{
    error::Error,
    fs::{self, File},
    io::{self, BufReader},
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use http::{Method, Request, StatusCode};
use oxiroute_config::{
    AlpnProtocol, Certificate, CertificateSource, DownstreamTimeoutPolicy, ForwardAuditMode,
    ForwardConnectPolicy, ForwardDestinationPolicy, ForwardHeaderPolicy, ForwardHttpVersion,
    ForwardPeerPolicy, ForwardProxyService, ForwardResolverPolicy, HttpPathSelector, HttpRoute,
    HttpRouteAction, HttpRoutePolicy, HttpService, Listener, ListenerBind, Protocol,
    TlsClientAuthMode, TlsClientAuthPolicy, TlsPolicy, TlsProfile, TlsVersion,
};
use quinn::crypto::rustls::{HandshakeData, QuicClientConfig};
use rustls::{
    ClientConfig, RootCertStore,
    pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
};
use tokio::time::{sleep, timeout};

use support::{
    TEST_TIMEOUT, TestOnlyEcdsaChain, generate_test_only_client_chain, private_key_fixture,
};

const H3_ALPN: &[u8] = b"h3";
const SERVER_NAME: &str = "proxy.example.test";

type TestResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

#[tokio::test]
async fn reverse_h3_client_auth_modes_are_bounded_at_quic_handshake() {
    timeout(TEST_TIMEOUT, async {
        for mode in [
            TlsClientAuthMode::Disabled,
            TlsClientAuthMode::Optional,
            TlsClientAuthMode::Required,
        ] {
            run_reverse_mode(mode).await;
        }
    })
    .await
    .expect("reverse H3 client-auth wire test timed out");
}

#[tokio::test]
async fn forward_h3_client_auth_modes_are_bounded_at_quic_handshake() {
    timeout(TEST_TIMEOUT, async {
        for mode in [
            TlsClientAuthMode::Disabled,
            TlsClientAuthMode::Optional,
            TlsClientAuthMode::Required,
        ] {
            run_forward_mode(mode).await;
        }
    })
    .await
    .expect("forward H3 client-auth wire test timed out");
}

async fn run_reverse_mode(mode: TlsClientAuthMode) {
    let valid_client = generate_test_only_client_chain("client.example.test");
    let disallowed_san = generate_test_only_client_chain("wrong.example.test");
    let wrong_ca = generate_test_only_client_chain("client.example.test");
    let ca_bundle = client_ca_bundle(&valid_client, &disallowed_san);
    let key = private_key_fixture("proxy-a-key.pem");
    let listener_address = reserve_udp_address();
    let mut config = reverse_config(
        listener_address,
        key.path(),
        mode,
        (mode != TlsClientAuthMode::Disabled).then_some(ca_bundle.as_path()),
    );
    oxiroute_config::validate_config(&mut config).expect("valid reverse H3 client-auth config");
    let server = process_support::ServerProcess::start(&config, None);

    let successful_clients = if mode == TlsClientAuthMode::Required {
        vec![Some(&valid_client)]
    } else {
        vec![None, Some(&valid_client)]
    };
    for client in successful_clients {
        let endpoint = client_endpoint(&fixture_support::fixture("ca-a.pem"), client, H3_ALPN)
            .expect("H3 client endpoint");
        let connection = connect_h3(&endpoint, listener_address)
            .await
            .expect("valid H3 client authentication");
        assert_h3_alpn(&connection);
        reverse_request(connection).await;
    }

    if mode == TlsClientAuthMode::Required {
        let endpoint = client_endpoint(&fixture_support::fixture("ca-a.pem"), None, H3_ALPN)
            .expect("unauthenticated H3 client endpoint");
        assert_h3_rejected("reverse absent certificate", &endpoint, listener_address).await;
    }

    if mode != TlsClientAuthMode::Disabled {
        let endpoint = client_endpoint(
            &fixture_support::fixture("ca-a.pem"),
            Some(&wrong_ca),
            H3_ALPN,
        )
        .expect("wrong-CA H3 client endpoint");
        assert_h3_rejected("reverse wrong CA", &endpoint, listener_address).await;

        let endpoint = client_endpoint(
            &fixture_support::fixture("ca-a.pem"),
            Some(&disallowed_san),
            H3_ALPN,
        )
        .expect("wrong-SAN H3 client endpoint");
        assert_h3_rejected("reverse disallowed SAN", &endpoint, listener_address).await;

        let endpoint = malformed_client_endpoint(
            &fixture_support::fixture("ca-a.pem"),
            &valid_client,
            H3_ALPN,
        )
        .expect("malformed-certificate H3 client endpoint");
        assert_h3_rejected("reverse malformed certificate", &endpoint, listener_address).await;
    }

    let endpoint = client_endpoint(&fixture_support::fixture("ca-a.pem"), None, b"not-h3")
        .expect("incompatible-ALPN H3 client endpoint");
    assert_h3_rejected("reverse incompatible ALPN", &endpoint, listener_address).await;

    server.shutdown_gracefully();
}

async fn run_forward_mode(mode: TlsClientAuthMode) {
    let valid_client = generate_test_only_client_chain("client.example.test");
    let disallowed_san = generate_test_only_client_chain("wrong.example.test");
    let wrong_ca = generate_test_only_client_chain("client.example.test");
    let ca_bundle = client_ca_bundle(&valid_client, &disallowed_san);
    let key = private_key_fixture("proxy-a-key.pem");
    let listener_address = reserve_udp_address();
    let mut config = forward_config(
        listener_address,
        key.path(),
        mode,
        (mode != TlsClientAuthMode::Disabled).then_some(ca_bundle.as_path()),
    );
    config.forward_proxy_services[0]
        .destination_policy
        .deny_private = false;
    oxiroute_config::validate_config(&mut config).expect("valid forward H3 client-auth config");
    let server = process_support::ServerProcess::start(&config, None);

    let successful_clients = if mode == TlsClientAuthMode::Required {
        vec![Some(&valid_client)]
    } else {
        vec![None, Some(&valid_client)]
    };
    for client in successful_clients {
        let endpoint = client_endpoint(&fixture_support::fixture("ca-a.pem"), client, H3_ALPN)
            .expect("H3 client endpoint");
        let connection = connect_h3(&endpoint, listener_address)
            .await
            .expect("valid H3 client authentication");
        assert_h3_alpn(&connection);
        connection.close(quinn::VarInt::from_u32(0), b"test complete");
    }

    if mode == TlsClientAuthMode::Required {
        let endpoint = client_endpoint(&fixture_support::fixture("ca-a.pem"), None, H3_ALPN)
            .expect("unauthenticated H3 client endpoint");
        assert_h3_rejected("forward absent certificate", &endpoint, listener_address).await;
    }

    if mode != TlsClientAuthMode::Disabled {
        let endpoint = client_endpoint(
            &fixture_support::fixture("ca-a.pem"),
            Some(&wrong_ca),
            H3_ALPN,
        )
        .expect("wrong-CA H3 client endpoint");
        assert_h3_rejected("forward wrong CA", &endpoint, listener_address).await;

        let endpoint = client_endpoint(
            &fixture_support::fixture("ca-a.pem"),
            Some(&disallowed_san),
            H3_ALPN,
        )
        .expect("wrong-SAN H3 client endpoint");
        assert_h3_rejected("forward disallowed SAN", &endpoint, listener_address).await;

        let endpoint = malformed_client_endpoint(
            &fixture_support::fixture("ca-a.pem"),
            &valid_client,
            H3_ALPN,
        )
        .expect("malformed-certificate H3 client endpoint");
        assert_h3_rejected("forward malformed certificate", &endpoint, listener_address).await;
    }

    let endpoint = client_endpoint(&fixture_support::fixture("ca-a.pem"), None, b"not-h3")
        .expect("incompatible-ALPN H3 client endpoint");
    assert_h3_rejected("forward incompatible ALPN", &endpoint, listener_address).await;

    server.shutdown_gracefully();
}

fn reverse_config(
    listener_address: SocketAddr,
    key_path: &Path,
    mode: TlsClientAuthMode,
    ca_bundle: Option<&Path>,
) -> oxiroute_config::Config {
    let mut config = support::empty_config();
    config.certificates.push(Certificate {
        name: "downstream".into(),
        dns_names: vec![SERVER_NAME.into()],
        source: CertificateSource::Files {
            certificate_chain_path: fixture_support::fixture("proxy-a.pem"),
            private_key_path: key_path.into(),
        },
    });
    config.tls_profiles.push(TlsProfile {
        name: "downstream".into(),
        certificates: vec!["downstream".into()],
        default_certificate: "downstream".into(),
        min_version: TlsVersion::Tls13,
        alpn: vec![AlpnProtocol::H3],
        policy: client_auth_policy(mode, ca_bundle),
    });
    config.http_services.push(HttpService {
        name: "web".into(),
        routes: vec![HttpRoute {
            host: None,
            path: HttpPathSelector::SegmentPrefix { value: "/".into() },
            methods: Vec::new(),
            access_policy: None,
            policy: HttpRoutePolicy {
                max_request_body_bytes: Some(64 * 1024),
                request_buffering: true,
                ..HttpRoutePolicy::default()
            },
            action: HttpRouteAction::FixedResponse {
                status: 200,
                body: "ok".into(),
                headers: Vec::new(),
            },
        }],
        automatic_response_headers: true,
        upstream_io_timeout_ms: 5_000,
        max_request_body_bytes: Some(64 * 1024),
        gzip: None,
        access_log: None,
    });
    config.listeners.push(Listener {
        name: "reverse".into(),
        bind: ListenerBind::Udp {
            address: listener_address,
        },
        protocol: Protocol::Http3,
        service: Some("web".into()),
        tls_profile: Some("downstream".into()),
        proxy_protocol: None,
        max_connections: Some(8),
        downstream_timeouts: DownstreamTimeoutPolicy::default(),
    });
    config
}

fn forward_config(
    listener_address: SocketAddr,
    key_path: &Path,
    mode: TlsClientAuthMode,
    ca_bundle: Option<&Path>,
) -> oxiroute_config::Config {
    let mut config = support::empty_config();
    config.certificates.push(Certificate {
        name: "downstream".into(),
        dns_names: vec![SERVER_NAME.into()],
        source: CertificateSource::Files {
            certificate_chain_path: fixture_support::fixture("proxy-a.pem"),
            private_key_path: key_path.into(),
        },
    });
    config.tls_profiles.push(TlsProfile {
        name: "downstream".into(),
        certificates: vec!["downstream".into()],
        default_certificate: "downstream".into(),
        min_version: TlsVersion::Tls13,
        alpn: vec![AlpnProtocol::H3],
        policy: client_auth_policy(mode, ca_bundle),
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
        destination_policy: ForwardDestinationPolicy::default(),
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
            address: listener_address,
        },
        protocol: Protocol::ForwardHttp3,
        service: Some("forward".into()),
        tls_profile: Some("downstream".into()),
        proxy_protocol: None,
        max_connections: None,
        downstream_timeouts: DownstreamTimeoutPolicy::default(),
    });
    config
}

fn client_auth_policy(mode: TlsClientAuthMode, ca_bundle: Option<&Path>) -> TlsPolicy {
    TlsPolicy {
        client_auth: match mode {
            TlsClientAuthMode::Disabled => TlsClientAuthPolicy::default(),
            TlsClientAuthMode::Optional | TlsClientAuthMode::Required => TlsClientAuthPolicy {
                mode,
                ca_certificate_path: ca_bundle.map(Path::to_path_buf),
                allowed_dns_names: vec!["client.example.test".into()],
            },
        },
        ..TlsPolicy::default()
    }
}

fn client_ca_bundle(valid: &TestOnlyEcdsaChain, disallowed_san: &TestOnlyEcdsaChain) -> PathBuf {
    let path = valid
        .root_certificate_path
        .with_file_name("h3-client-ca-bundle.pem");
    let mut bytes = fs::read(&valid.root_certificate_path).expect("valid client CA");
    bytes.extend_from_slice(&fs::read(&disallowed_san.root_certificate_path).expect("SAN CA"));
    fs::write(&path, bytes).expect("H3 client CA bundle");
    path
}

fn reserve_udp_address() -> SocketAddr {
    std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("reserve UDP address")
        .local_addr()
        .expect("reserved UDP address")
}

fn client_endpoint(
    server_ca_path: &Path,
    client: Option<&TestOnlyEcdsaChain>,
    alpn: &[u8],
) -> TestResult<quinn::Endpoint> {
    let config = match client {
        Some(client) => support::tls_client_config_with_identity(
            server_ca_path,
            &client.fullchain_path,
            &client.leaf_private_key_path,
            &[alpn],
        )?,
        None => support::tls_client_config(server_ca_path, &[alpn])?,
    };
    quic_client_endpoint((*config).clone())
}

fn malformed_client_endpoint(
    server_ca_path: &Path,
    client: &TestOnlyEcdsaChain,
    alpn: &[u8],
) -> TestResult<quinn::Endpoint> {
    let mut roots = RootCertStore::empty();
    let ca = fs::read(server_ca_path)?;
    for certificate in CertificateDer::pem_slice_iter(&ca) {
        roots.add(certificate?)?;
    }

    let mut certificate_reader = BufReader::new(File::open(&client.fullchain_path)?);
    let mut certificates =
        CertificateDer::pem_reader_iter(&mut certificate_reader).collect::<Result<Vec<_>, _>>()?;
    certificates.truncate(1);
    certificates.push(CertificateDer::from(b"malformed client issuer".to_vec()));
    let mut key_reader = BufReader::new(File::open(&client.leaf_private_key_path)?);
    let private_key = PrivateKeyDer::pem_reader_iter(&mut key_reader)
        .next()
        .transpose()?
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing client key"))?;
    let mut config = ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_root_certificates(roots)
        .with_client_auth_cert(certificates, private_key)?;
    config.alpn_protocols = vec![alpn.to_vec()];
    quic_client_endpoint(config)
}

fn quic_client_endpoint(config: ClientConfig) -> TestResult<quinn::Endpoint> {
    let crypto = QuicClientConfig::try_from(config).map_err(io::Error::other)?;
    let client_config = quinn::ClientConfig::new(Arc::new(crypto));
    let mut endpoint = quinn::Endpoint::client((Ipv4Addr::LOCALHOST, 0).into())?;
    endpoint.set_default_client_config(client_config);
    Ok(endpoint)
}

async fn connect_h3(
    endpoint: &quinn::Endpoint,
    listener_address: SocketAddr,
) -> TestResult<quinn::Connection> {
    timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(connecting) = endpoint.connect(listener_address, SERVER_NAME)
                && let Ok(connection) = connecting.await
            {
                return Ok(connection);
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|error| io::Error::new(io::ErrorKind::TimedOut, error))?
}

async fn assert_h3_rejected(label: &str, endpoint: &quinn::Endpoint, listener_address: SocketAddr) {
    let rejected = match endpoint.connect(listener_address, SERVER_NAME) {
        Ok(connecting) => match timeout(Duration::from_secs(3), connecting).await {
            Ok(Ok(connection)) => timeout(Duration::from_secs(3), connection.closed())
                .await
                .is_ok(),
            Ok(Err(_)) | Err(_) => true,
        },
        Err(_) => true,
    };
    assert!(rejected, "{label} completed a QUIC handshake");
}

fn assert_h3_alpn(connection: &quinn::Connection) {
    let handshake = connection
        .handshake_data()
        .expect("H3 handshake data")
        .downcast::<HandshakeData>()
        .expect("rustls H3 handshake data");
    assert_eq!(handshake.protocol.as_deref(), Some(H3_ALPN));
}

async fn reverse_request(connection: quinn::Connection) {
    let raw_connection = connection.clone();
    let (mut driver, mut sender) = h3::client::new(h3_quinn::Connection::new(connection))
        .await
        .expect("H3 client connection");
    let driver = tokio::spawn(async move {
        let _ = std::future::poll_fn(|context| driver.poll_close(context)).await;
    });
    let mut stream = sender
        .send_request(
            Request::builder()
                .method(Method::GET)
                .uri("https://example.test/auth")
                .body(())
                .expect("reverse H3 request"),
        )
        .await
        .expect("send reverse H3 request");
    stream.finish().await.expect("finish reverse H3 request");
    assert_eq!(
        stream
            .recv_response()
            .await
            .expect("reverse H3 response")
            .status(),
        StatusCode::OK
    );
    assert!(stream.recv_data().await.expect("reverse H3 body").is_some());
    drop(stream);
    drop(sender);
    raw_connection.close(quinn::VarInt::from_u32(0), b"test complete");
    driver.await.expect("reverse H3 driver");
}
