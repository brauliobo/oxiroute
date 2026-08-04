#![cfg(unix)]

mod support;

use std::{fs, net::SocketAddr, os::unix::fs::PermissionsExt as _, sync::Arc};

use bytes::Bytes;
use http::{Method, StatusCode};
use oxiroute_config::{
    AlpnProtocol, Certificate, CertificateSource, HttpLiteralHeader, HttpRequestHeaderMutation,
    HttpRequestHeaderValue, HttpRetryPolicy, HttpRetryTrigger, HttpRouteAction,
    HttpStaticErrorResponse, HttpStaticMimePolicy, HttpStaticPathMapping, HttpVersion,
    HttpVersionPolicy, TlsClientAuthMode, TlsClientAuthPolicy, TlsVersion, UpstreamServer,
    UpstreamTls,
};
use oxiroute_server::CertificateGeneration;
use rustls::{HandshakeKind, ProtocolVersion};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinSet,
    time::{sleep, timeout},
};

use support::{
    GRPC_BODY, H2Client, LegacyTlsOrigin, LegacyTlsVersion, ORIGIN_SERVER_NAME, PROXY_SERVER_NAME,
    PlainH1Origin, ProxyHarness, ReservedListener, TEST_TIMEOUT, TestCertbotLineage, TlsOrigin,
    certificate_chain_fixture, direct_legacy_tls_origin_handshake, fixture, fixture_leaf,
    generate_test_only_client_chain, generate_test_only_ecdsa_chain, h1_request, handshake_kind,
    legacy_tls_handshake, negotiated_alpn, negotiated_tls_is_modern, peer_certificate_count,
    openssl_tls_request_with_identity, peer_leaf, private_key_fixture, proxy_config,
    socket_endpoint, tcp_connect, tls_client_config, tls_client_config_with_identity,
    tls_client_config_with_versions, tls_connect, tls_connect_with_config, verified_upstream,
};

const TENANT_SNI_SERVER_NAME: &str = "tenant.sni.example.test";

#[tokio::test]
async fn downstream_tls_h1_uses_runtime_profile_and_pingora_listener() {
    timeout(TEST_TIMEOUT, async {
        let origin = PlainH1Origin::start(b"downstream-h1").await;
        let reserved = ReservedListener::new();
        let config = proxy_config(
            reserved.address,
            origin.address,
            vec![AlpnProtocol::Http11],
            None,
            HttpVersionPolicy::default(),
        );
        let proxy = ProxyHarness::start(&config, reserved);

        assert!(
            tls_connect(
                proxy.address,
                "wrong.example.test",
                "ca-a.pem",
                &[b"http/1.1"],
            )
            .await
            .is_err(),
            "rustls must reject the downstream certificate for the wrong DNS name"
        );

        let mut stream = tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"http/1.1"])
            .await
            .expect("rustls connection to OxiRoute");
        assert!(negotiated_tls_is_modern(&stream));
        assert_eq!(negotiated_alpn(&stream), Some(b"http/1.1".as_slice()));

        let response = h1_request(&mut stream, "/h1", true)
            .await
            .expect("proxied H1 response");
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"downstream-h1");
        origin.wait_for_requests(1).await;

        drop(stream);
        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("downstream TLS/H1 wire test timed out");
}

#[tokio::test]
async fn required_downstream_client_auth_rejects_no_certificate_before_http() {
    timeout(TEST_TIMEOUT, async {
        let client_identity = generate_test_only_client_chain("client.example.test");
        let wrong_san_identity = generate_test_only_client_chain("wrong.example.test");
        let ca_bundle = client_identity
            .root_certificate_path
            .with_file_name("client-ca-bundle.pem");
        let mut ca_bytes = fs::read(&client_identity.root_certificate_path).expect("client CA");
        ca_bytes.extend_from_slice(
            &fs::read(&wrong_san_identity.root_certificate_path).expect("wrong SAN CA"),
        );
        fs::write(&ca_bundle, ca_bytes).expect("client CA bundle");

        let origin = PlainH1Origin::start(b"client-auth-required").await;
        let reserved = ReservedListener::new();
        let mut config = proxy_config(
            reserved.address,
            origin.address,
            vec![AlpnProtocol::Http11],
            None,
            HttpVersionPolicy::default(),
        );
        config.tls_profiles[0].policy.client_auth = TlsClientAuthPolicy {
            mode: TlsClientAuthMode::Required,
            ca_certificate_path: Some(ca_bundle),
            allowed_dns_names: vec!["client.example.test".into()],
        };
        let proxy = ProxyHarness::start(&config, reserved);

        let no_certificate_rejected = match
            tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"http/1.1"]).await
        {
            Ok(mut stream) => h1_request(&mut stream, "/without-client-cert", true)
                .await
                .is_err(),
            Err(_) => true,
        };
        assert!(
            no_certificate_rejected,
            "required client auth accepted no certificate"
        );
        assert_eq!(origin.requests(), 0);
        assert_eq!(origin.http_bytes(), 0);

        let client_config = tls_client_config_with_identity(
            &fixture("ca-a.pem"),
            &client_identity.fullchain_path,
            &client_identity.leaf_private_key_path,
            &[b"http/1.1"],
        )
        .expect("valid client certificate config");
        let mut stream = tls_connect_with_config(proxy.address, PROXY_SERVER_NAME, client_config)
            .await
            .expect("required client auth accepted valid certificate");
        let response = h1_request(&mut stream, "/client-auth-required", true)
            .await
            .expect("required client auth response");
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"client-auth-required");
        origin.wait_for_requests(1).await;

        drop(stream);
        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("required client auth wire test timed out");
}

#[tokio::test]
async fn optional_downstream_client_auth_validates_chain_and_san_when_present() {
    timeout(TEST_TIMEOUT, async {
        let client_identity = generate_test_only_client_chain("client.example.test");
        let wrong_san_identity = generate_test_only_client_chain("wrong.example.test");
        let wrong_ca_identity = generate_test_only_client_chain("client.example.test");
        let ca_bundle = client_identity
            .root_certificate_path
            .with_file_name("client-ca-bundle.pem");
        let mut ca_bytes = fs::read(&client_identity.root_certificate_path).expect("client CA");
        ca_bytes.extend_from_slice(
            &fs::read(&wrong_san_identity.root_certificate_path).expect("wrong SAN CA"),
        );
        fs::write(&ca_bundle, ca_bytes).expect("client CA bundle");

        let origin = PlainH1Origin::start(b"client-auth-optional").await;
        let reserved = ReservedListener::new();
        let mut config = proxy_config(
            reserved.address,
            origin.address,
            vec![AlpnProtocol::Http11],
            None,
            HttpVersionPolicy::default(),
        );
        config.tls_profiles[0].policy.client_auth = TlsClientAuthPolicy {
            mode: TlsClientAuthMode::Optional,
            ca_certificate_path: Some(ca_bundle),
            allowed_dns_names: vec!["client.example.test".into()],
        };
        let proxy = ProxyHarness::start(&config, reserved);

        let mut no_certificate =
            tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"http/1.1"])
                .await
                .expect("optional client auth accepted no certificate");
        let response = h1_request(&mut no_certificate, "/without-client-cert", true)
            .await
            .expect("optional no-certificate response");
        assert_eq!(response.status, 200);

        let wrong_ca_result = openssl_tls_request_with_identity(
            proxy.address,
            PROXY_SERVER_NAME,
            &fixture("ca-a.pem"),
            &wrong_ca_identity.fullchain_path,
            &wrong_ca_identity.leaf_private_key_path,
        )
        .await;
        assert!(
            !matches!(wrong_ca_result, Ok(true)),
            "optional client auth accepted a certificate from the wrong CA"
        );

        let wrong_san_config = tls_client_config_with_identity(
            &fixture("ca-a.pem"),
            &wrong_san_identity.fullchain_path,
            &wrong_san_identity.leaf_private_key_path,
            &[b"http/1.1"],
        )
        .expect("wrong SAN client certificate config");
        let wrong_san_rejected = match
            tls_connect_with_config(proxy.address, PROXY_SERVER_NAME, wrong_san_config).await
        {
            Ok(mut stream) => h1_request(&mut stream, "/wrong-san", true).await.is_err(),
            Err(_) => true,
        };
        assert!(
            wrong_san_rejected,
            "optional client auth accepted a certificate with a disallowed SAN"
        );

        let valid_config = tls_client_config_with_identity(
            &fixture("ca-a.pem"),
            &client_identity.fullchain_path,
            &client_identity.leaf_private_key_path,
            &[b"http/1.1"],
        )
        .expect("valid optional client certificate config");
        let mut valid = tls_connect_with_config(proxy.address, PROXY_SERVER_NAME, valid_config)
            .await
            .expect("optional client auth accepted valid certificate");
        let response = h1_request(&mut valid, "/with-client-cert", true)
            .await
            .expect("optional client certificate response");
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"client-auth-optional");
        origin.wait_for_requests(2).await;

        drop(valid);
        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("optional client auth wire test timed out");
}

#[tokio::test]
async fn production_listener_populates_the_client_ip_request_header_variable() {
    timeout(TEST_TIMEOUT, async {
        let origin = TlsOrigin::start_h1(b"client-ip").await;
        let reserved = ReservedListener::new();
        let mut config = proxy_config(
            reserved.address,
            origin.address,
            vec![AlpnProtocol::Http11],
            Some(verified_upstream(ORIGIN_SERVER_NAME, "ca-a.pem")),
            HttpVersionPolicy::default(),
        );
        let HttpRouteAction::Proxy { policy, .. } = &mut config.http_services[0].routes[0].action
        else {
            panic!("wire route must proxy");
        };
        policy.request_headers = vec![HttpRequestHeaderMutation::Set {
            name: "x-client-ip".into(),
            value: HttpRequestHeaderValue::ClientIp,
        }];
        let proxy = ProxyHarness::start(&config, reserved);
        let mut client = tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"http/1.1"])
            .await
            .expect("client-IP downstream TLS connect");

        let response = h1_request(&mut client, "/client-ip", true)
            .await
            .expect("client-IP response");
        assert_eq!(response.status, 200);
        origin.observations.wait_for_http_requests(1).await;
        let requests = origin.observations.request_heads();
        let request = String::from_utf8_lossy(&requests[0]).to_ascii_lowercase();
        assert!(
            request.contains("\r\nx-client-ip: 127.0.0.1\r\n"),
            "request: {request}"
        );

        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("client-IP request-header test timed out");
}

#[tokio::test]
async fn downstream_serves_ecdsa_leaf_and_intermediate_to_a_root_only_rustls_client() {
    timeout(TEST_TIMEOUT, async {
        let chain = generate_test_only_ecdsa_chain(PROXY_SERVER_NAME);
        let origin = PlainH1Origin::start(b"ecdsa-fullchain").await;
        let reserved = ReservedListener::new();
        let mut config = proxy_config(
            reserved.address,
            origin.address,
            vec![AlpnProtocol::Http11],
            None,
            HttpVersionPolicy::default(),
        );
        config.certificates[0] = Certificate {
            name: "downstream".into(),
            dns_names: vec![PROXY_SERVER_NAME.into()],
            source: CertificateSource::Files {
                certificate_chain_path: chain.fullchain_path.clone(),
                private_key_path: chain.leaf_private_key_path.clone(),
            },
        };
        let proxy = ProxyHarness::start(&config, reserved);
        assert_eq!(
            proxy
                .active_certificate
                .snapshot()
                .metadata()
                .intermediate_count,
            1
        );

        let client_config = tls_client_config(&chain.root_certificate_path, &[b"http/1.1"])
            .expect("root-only rustls client config");
        let mut stream = tls_connect_with_config(proxy.address, PROXY_SERVER_NAME, client_config)
            .await
            .expect("root-only rustls fullchain handshake");
        assert_eq!(peer_certificate_count(&stream), 2);
        assert_eq!(peer_leaf(&stream), chain.leaf_der);
        let response = h1_request(&mut stream, "/ecdsa-fullchain", true)
            .await
            .expect("request over ECDSA fullchain connection");
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"ecdsa-fullchain");
        origin.wait_for_requests(1).await;

        drop(stream);
        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("downstream ECDSA fullchain wire test timed out");
}

#[tokio::test]
async fn downstream_selects_exact_wildcard_and_default_certificates_by_sni() {
    timeout(TEST_TIMEOUT, async {
        const EXACT_SERVER_NAME: &str = "api.sni.example.test";
        const WILDCARD_DNS_NAME: &str = "*.sni.example.test";
        const WILDCARD_SERVER_NAME: &str = "www.sni.example.test";

        let wildcard = generate_test_only_ecdsa_chain(WILDCARD_DNS_NAME);
        let exact = generate_test_only_ecdsa_chain(EXACT_SERVER_NAME);
        let origin = PlainH1Origin::start(b"sni-selection").await;
        let reserved = ReservedListener::new();
        let mut config = proxy_config(
            reserved.address,
            origin.address,
            vec![AlpnProtocol::Http11],
            None,
            HttpVersionPolicy::default(),
        );
        config.certificates.extend([
            Certificate {
                name: "wildcard".into(),
                dns_names: vec![WILDCARD_DNS_NAME.into()],
                source: CertificateSource::Files {
                    certificate_chain_path: wildcard.fullchain_path.clone(),
                    private_key_path: wildcard.leaf_private_key_path.clone(),
                },
            },
            Certificate {
                name: "exact".into(),
                dns_names: vec![EXACT_SERVER_NAME.into()],
                source: CertificateSource::Files {
                    certificate_chain_path: exact.fullchain_path.clone(),
                    private_key_path: exact.leaf_private_key_path.clone(),
                },
            },
        ]);
        config.tls_profiles[0]
            .certificates
            .extend(["wildcard".into(), "exact".into()]);
        let proxy = ProxyHarness::start(&config, reserved);

        let exact_client = tls_client_config(&exact.root_certificate_path, &[b"http/1.1"])
            .expect("exact-name rustls client config");
        let mut exact_stream =
            tls_connect_with_config(proxy.address, EXACT_SERVER_NAME, exact_client)
                .await
                .expect("exact-name SNI connection");
        assert_eq!(peer_leaf(&exact_stream), exact.leaf_der);
        assert_eq!(
            h1_request(&mut exact_stream, "/exact", true)
                .await
                .expect("exact-name request")
                .body,
            b"sni-selection"
        );

        let wildcard_client = tls_client_config(&wildcard.root_certificate_path, &[b"http/1.1"])
            .expect("wildcard rustls client config");
        let mut wildcard_stream =
            tls_connect_with_config(proxy.address, WILDCARD_SERVER_NAME, wildcard_client)
                .await
                .expect("wildcard SNI connection");
        assert_eq!(peer_leaf(&wildcard_stream), wildcard.leaf_der);
        assert_eq!(
            h1_request(&mut wildcard_stream, "/wildcard", true)
                .await
                .expect("wildcard request")
                .body,
            b"sni-selection"
        );

        let mut no_sni_client = tls_client_config(&fixture("ca-a.pem"), &[b"http/1.1"])
            .expect("default-certificate rustls client config");
        Arc::get_mut(&mut no_sni_client)
            .expect("unique rustls client config")
            .enable_sni = false;
        let mut default_stream =
            tls_connect_with_config(proxy.address, PROXY_SERVER_NAME, no_sni_client)
                .await
                .expect("no-SNI default-certificate connection");
        assert_eq!(peer_leaf(&default_stream), fixture_leaf("proxy-a.pem"));
        assert_eq!(
            h1_request(&mut default_stream, "/default", true)
                .await
                .expect("default-certificate request")
                .body,
            b"sni-selection"
        );
        origin.wait_for_requests(3).await;

        drop(exact_stream);
        drop(wildcard_stream);
        drop(default_stream);
        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("downstream SNI certificate-selection wire test timed out");
}

#[tokio::test]
async fn downstream_connection_cap_includes_incomplete_tls_handshakes() {
    timeout(TEST_TIMEOUT, async {
        let origin = PlainH1Origin::start(b"admission").await;
        let reserved = ReservedListener::new();
        let mut config = proxy_config(
            reserved.address,
            origin.address,
            vec![AlpnProtocol::Http11],
            None,
            HttpVersionPolicy::default(),
        );
        config.listeners[0].max_connections = Some(1);
        let proxy = ProxyHarness::start(&config, reserved);

        let incomplete = tcp_connect(proxy.address)
            .await
            .expect("incomplete TLS connection");
        proxy.wait_for_active_connections(1).await;
        assert!(
            tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"http/1.1"])
                .await
                .is_err(),
            "a second connection must be rejected before its TLS handshake"
        );
        proxy.wait_for_active_connections(1).await;

        drop(incomplete);
        proxy.wait_for_active_connections(0).await;
        let mut admitted =
            tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"http/1.1"])
                .await
                .expect("connection after handshake slot release");
        let response = h1_request(&mut admitted, "/admitted", true)
            .await
            .expect("admitted request");
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"admission");
        origin.wait_for_requests(1).await;

        drop(admitted);
        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("pre-handshake connection admission wire test timed out");
}

#[tokio::test]
async fn downstream_refuses_tls_1_0_and_1_1_before_any_http_bytes() {
    timeout(TEST_TIMEOUT, async {
        let origin = PlainH1Origin::start(b"must-not-be-read").await;
        let reserved = ReservedListener::new();
        let config = proxy_config(
            reserved.address,
            origin.address,
            vec![AlpnProtocol::Http11],
            None,
            HttpVersionPolicy::default(),
        );
        let proxy = ProxyHarness::start(&config, reserved);

        for version in [LegacyTlsVersion::Tls10, LegacyTlsVersion::Tls11] {
            let rejected = legacy_tls_handshake(proxy.address, version)
                .await
                .unwrap_or_else(|error| panic!("{version:?} client attempt failed: {error}"));
            assert!(
                rejected.error.contains("protocol version"),
                "{version:?} failed for the wrong reason: {}",
                rejected.error
            );
            assert!(
                !rejected.server_bytes.is_empty(),
                "{version:?} client must receive a bounded TLS rejection"
            );
            assert!(
                !rejected
                    .server_bytes
                    .windows(b"HTTP/".len())
                    .any(|window| window == b"HTTP/"),
                "{version:?} rejection exposed HTTP bytes"
            );
        }
        sleep(std::time::Duration::from_millis(25)).await;
        assert_eq!(origin.http_bytes(), 0);
        assert_eq!(origin.requests(), 0);

        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("downstream legacy TLS refusal wire test timed out");
}

#[tokio::test]
async fn downstream_tls_1_3_minimum_rejects_tls_1_2_and_accepts_tls_1_3() {
    timeout(TEST_TIMEOUT, async {
        let origin = PlainH1Origin::start(b"tls13-minimum").await;
        let reserved = ReservedListener::new();
        let mut config = proxy_config(
            reserved.address,
            origin.address,
            vec![AlpnProtocol::Http11],
            None,
            HttpVersionPolicy::default(),
        );
        config.tls_profiles[0].min_version = TlsVersion::Tls13;
        let proxy = ProxyHarness::start(&config, reserved);

        let tls12 = tls_client_config_with_versions(
            &fixture("ca-a.pem"),
            &[b"http/1.1"],
            &[&rustls::version::TLS12],
        )
        .expect("TLS 1.2-only client config");
        assert!(
            tls_connect_with_config(proxy.address, PROXY_SERVER_NAME, tls12)
                .await
                .is_err(),
            "TLS 1.2 must not satisfy a TLS 1.3 listener minimum"
        );

        let tls13 = tls_client_config_with_versions(
            &fixture("ca-a.pem"),
            &[b"http/1.1"],
            &[&rustls::version::TLS13],
        )
        .expect("TLS 1.3-only client config");
        let mut stream = tls_connect_with_config(proxy.address, PROXY_SERVER_NAME, tls13)
            .await
            .expect("TLS 1.3 downstream connection");
        assert_eq!(
            stream.get_ref().1.protocol_version(),
            Some(ProtocolVersion::TLSv1_3)
        );
        let response = h1_request(&mut stream, "/tls13", true)
            .await
            .expect("TLS 1.3 proxied response");
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"tls13-minimum");
        origin.wait_for_requests(1).await;

        drop(stream);
        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("downstream TLS 1.3 minimum wire test timed out");
}

#[tokio::test]
async fn downstream_tls_h2_negotiates_alpn_and_proxies_a_real_stream() {
    timeout(TEST_TIMEOUT, async {
        let origin = PlainH1Origin::start(b"downstream-h2").await;
        let reserved = ReservedListener::new();
        let config = proxy_config(
            reserved.address,
            origin.address,
            vec![AlpnProtocol::H2, AlpnProtocol::Http11],
            None,
            HttpVersionPolicy::default(),
        );
        let proxy = ProxyHarness::start(&config, reserved);

        let stream = tls_connect(
            proxy.address,
            PROXY_SERVER_NAME,
            "ca-a.pem",
            &[b"h2", b"http/1.1"],
        )
        .await
        .expect("rustls H2 connection to OxiRoute");
        assert!(negotiated_tls_is_modern(&stream));
        assert_eq!(negotiated_alpn(&stream), Some(b"h2".as_slice()));
        let mut client = H2Client::from_tls(stream)
            .await
            .expect("H2 client handshake");

        let response = client
            .request(Method::GET, "/h2-downstream")
            .await
            .expect("proxied H2 response");
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.body, b"downstream-h2".as_slice());
        assert!(response.trailers.is_none());
        origin.wait_for_requests(1).await;

        client.finish().await;
        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("downstream TLS/H2 wire test timed out");
}

#[tokio::test]
async fn downstream_h2_upload_streams_to_plain_h1_upstream() {
    timeout(TEST_TIMEOUT, async {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("H2 upload origin bind");
        let origin_address = listener.local_addr().expect("H2 upload origin address");
        let origin = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("H2 upload origin accept");
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            while !request.windows(5).any(|window| window == b"0\r\n\r\n") {
                let read = stream
                    .read(&mut buffer)
                    .await
                    .expect("H2 upload origin read");
                assert!(read > 0, "H2 upload ended before the terminating chunk");
                request.extend_from_slice(&buffer[..read]);
            }
            let request_text = String::from_utf8_lossy(&request).to_ascii_lowercase();
            assert!(request_text.starts_with("post /upload http/1.1\r\n"));
            assert!(request_text.contains("transfer-encoding: chunked\r\n"));
            let (chunks, body) = parse_chunked_request_body(&request);
            assert!(
                chunks >= 2,
                "multiple H2 DATA frames were collapsed upstream"
            );
            assert_eq!(body, b"h2-multi-data-upload");
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\nupload-ok",
                )
                .await
                .expect("H2 upload origin response");
        });
        let reserved = ReservedListener::new();
        let config = proxy_config(
            reserved.address,
            origin_address,
            vec![AlpnProtocol::H2, AlpnProtocol::Http11],
            None,
            HttpVersionPolicy::default(),
        );
        let proxy = ProxyHarness::start(&config, reserved);

        let stream = tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"h2"])
            .await
            .expect("H2 upload client connection");
        let mut client = H2Client::from_tls(stream)
            .await
            .expect("H2 upload client handshake");
        let response = client
            .request_with_body_chunks(
                Method::POST,
                "/upload",
                vec![
                    Bytes::from_static(b"h2-"),
                    Bytes::from_static(b"multi-"),
                    Bytes::from_static(b"data-"),
                    Bytes::from_static(b"upload"),
                ],
            )
            .await
            .expect("H2 upload response");
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.body, b"upload-ok".as_slice());

        origin.await.expect("H2 upload origin task");
        client.finish().await;
        proxy.finish().await;
    })
    .await
    .expect("downstream H2 upload test timed out");
}

fn parse_chunked_request_body(request: &[u8]) -> (usize, Vec<u8>) {
    let mut cursor = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("H2 upload request header terminator")
        + 4;
    let mut chunks = 0;
    let mut body = Vec::new();
    loop {
        let line_end = request[cursor..]
            .windows(2)
            .position(|window| window == b"\r\n")
            .expect("H1 chunk size terminator")
            + cursor;
        let size = usize::from_str_radix(
            std::str::from_utf8(&request[cursor..line_end])
                .expect("ASCII H1 chunk size")
                .split(';')
                .next()
                .unwrap(),
            16,
        )
        .expect("hex H1 chunk size");
        cursor = line_end + 2;
        if size == 0 {
            return (chunks, body);
        }
        chunks += 1;
        body.extend_from_slice(&request[cursor..cursor + size]);
        cursor += size;
        assert_eq!(&request[cursor..cursor + 2], b"\r\n");
        cursor += 2;
    }
}

#[tokio::test]
async fn downstream_h2_executes_fixed_actions_with_head_semantics_without_an_upstream() {
    timeout(TEST_TIMEOUT, async {
        let unused_origin =
            std::net::TcpListener::bind("127.0.0.1:0").expect("reserve unused action origin");
        unused_origin
            .set_nonblocking(true)
            .expect("unused action origin nonblocking");
        let origin_address = unused_origin.local_addr().expect("unused origin address");
        let reserved = ReservedListener::new();
        let mut config = proxy_config(
            reserved.address,
            origin_address,
            vec![AlpnProtocol::H2],
            None,
            HttpVersionPolicy::default(),
        );
        config.http_services[0].routes[0].action = HttpRouteAction::FixedResponse {
            status: 200,
            body: "h2-fixed".into(),
            headers: vec![HttpLiteralHeader {
                name: "x-action".into(),
                value: "fixed".into(),
                always: true,
            }],
        };
        let proxy = ProxyHarness::start(&config, reserved);
        let stream = tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"h2"])
            .await
            .expect("H2 action TLS connect");
        let mut client = H2Client::from_tls(stream).await.expect("H2 action client");

        let get = client
            .request(Method::GET, "/fixed")
            .await
            .expect("H2 fixed GET");
        assert_eq!(get.status, StatusCode::OK);
        assert_eq!(get.headers.get("x-action").unwrap(), "fixed");
        assert_eq!(get.headers.get("content-length").unwrap(), "8");
        assert_eq!(get.body, "h2-fixed");

        let head = client
            .request(Method::HEAD, "/fixed")
            .await
            .expect("H2 fixed HEAD");
        assert_eq!(head.status, StatusCode::OK);
        assert_eq!(head.headers.get("content-length").unwrap(), "8");
        assert!(head.body.is_empty());

        client.finish().await;
        proxy.finish().await;
        assert!(unused_origin.accept().is_err());
    })
    .await
    .expect("H2 fixed action test timed out");
}

#[tokio::test]
async fn downstream_h2_serves_static_headers_and_internal_error_page() {
    timeout(TEST_TIMEOUT, async {
        let directory = tempfile::tempdir().expect("H2 static response directory");
        let root = directory.path().join("public");
        fs::create_dir(&root).expect("create H2 static response root");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
            .expect("secure H2 static response root");
        fs::write(root.join("ok.txt"), b"ok").expect("write H2 static success file");
        fs::write(root.join("404.html"), b"missing").expect("write H2 static error file");

        let unused_origin =
            std::net::TcpListener::bind("127.0.0.1:0").expect("reserve unused H2 static origin");
        unused_origin
            .set_nonblocking(true)
            .expect("unused H2 static origin nonblocking");
        let origin_address = unused_origin
            .local_addr()
            .expect("unused H2 static origin address");
        let reserved = ReservedListener::new();
        let mut config = proxy_config(
            reserved.address,
            origin_address,
            vec![AlpnProtocol::H2],
            None,
            HttpVersionPolicy::default(),
        );
        config.http_services[0].routes[0].action = HttpRouteAction::StaticFiles {
            root_directory: root,
            path_mapping: HttpStaticPathMapping::Root,
            index_files: Vec::new(),
            internal_index_redirects: true,
            directory_redirects: true,
            spa_fallback: None,
            try_files: Vec::new(),
            autoindex: false,
            autoindex_exact_size: true,
            autoindex_local_time: false,
            etag: true,
            mime: HttpStaticMimePolicy {
                default_type: Some("text/plain".into()),
                types: Vec::new(),
            },
            headers: vec![
                HttpLiteralHeader {
                    name: "x-selected-status".into(),
                    value: "no".into(),
                    always: false,
                },
                HttpLiteralHeader {
                    name: "x-always".into(),
                    value: "yes".into(),
                    always: true,
                },
            ],
            error_responses: vec![HttpStaticErrorResponse {
                statuses: vec![404],
                file: Some("404.html".into()),
                body: None,
                headers: vec![HttpLiteralHeader {
                    name: "x-error".into(),
                    value: "yes".into(),
                    always: true,
                }],
                internal_redirect: Some("/404.html".into()),
            }],
        };
        let proxy = ProxyHarness::start(&config, reserved);
        let stream = tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"h2"])
            .await
            .expect("H2 static response TLS connect");
        let mut client = H2Client::from_tls(stream)
            .await
            .expect("H2 static response client");

        let success = client
            .request(Method::GET, "/ok.txt")
            .await
            .expect("H2 static success");
        assert_eq!(success.status, StatusCode::OK);
        assert_eq!(success.headers.get("x-selected-status").unwrap(), "no");
        assert_eq!(success.headers.get("x-always").unwrap(), "yes");
        assert!(!success.headers.contains_key("x-error"));
        assert_eq!(success.body.as_ref(), b"ok");

        let missing = client
            .request(Method::GET, "/missing")
            .await
            .expect("H2 static error page");
        assert_eq!(missing.status, StatusCode::NOT_FOUND);
        assert!(!missing.headers.contains_key("x-selected-status"));
        assert_eq!(missing.headers.get("x-always").unwrap(), "yes");
        assert_eq!(missing.headers.get("x-error").unwrap(), "yes");
        assert_eq!(missing.body.as_ref(), b"missing");

        client.finish().await;
        proxy.finish().await;
        assert!(unused_origin.accept().is_err());
    })
    .await
    .expect("static response H2 test timed out");
}

#[tokio::test]
async fn downstream_h2_only_rejects_no_alpn_and_h1_before_http_but_allows_h2() {
    timeout(TEST_TIMEOUT, async {
        let origin = PlainH1Origin::start(b"h2-only-downstream").await;
        let reserved = ReservedListener::new();
        let config = proxy_config(
            reserved.address,
            origin.address,
            vec![AlpnProtocol::H2],
            None,
            HttpVersionPolicy::default(),
        );
        let proxy = ProxyHarness::start(&config, reserved);

        // OpenSSL does not invoke the ALPN selection callback when the ClientHello omits ALPN, so
        // TLS can complete. The production listener wrapper must close before HTTP/1 parsing.
        let mut no_alpn = tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[])
            .await
            .expect("no-ALPN TLS handshake may complete");
        assert_eq!(negotiated_alpn(&no_alpn), None);
        assert!(
            h1_request(&mut no_alpn, "/must-not-parse", true)
                .await
                .is_err()
        );

        assert!(
            tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"http/1.1"],)
                .await
                .is_err(),
            "an explicit incompatible ALPN offer must receive a fatal handshake failure"
        );
        sleep(std::time::Duration::from_millis(25)).await;
        assert_eq!(origin.requests(), 0);
        assert_eq!(origin.http_bytes(), 0);

        let stream = tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"h2"])
            .await
            .expect("valid H2-only downstream handshake");
        assert_eq!(negotiated_alpn(&stream), Some(b"h2".as_slice()));
        let mut client = H2Client::from_tls(stream)
            .await
            .expect("valid H2-only downstream client");
        let response = client
            .request(Method::GET, "/valid-h2")
            .await
            .expect("valid H2-only proxied request");
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.body, b"h2-only-downstream".as_slice());
        origin.wait_for_requests(1).await;
        assert_eq!(origin.requests(), 1);

        client.finish().await;
        drop(no_alpn);
        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("downstream H2-only enforcement wire test timed out");
}

#[tokio::test]
async fn upstream_tls_verifies_server_name_and_custom_ca_before_http() {
    timeout(TEST_TIMEOUT, async {
        let trusted = TlsOrigin::start_h1(b"verified-upstream").await;
        let reserved = ReservedListener::new();
        let config = proxy_config(
            reserved.address,
            trusted.address,
            vec![AlpnProtocol::Http11],
            Some(verified_upstream(ORIGIN_SERVER_NAME, "ca-a.pem")),
            HttpVersionPolicy::default(),
        );
        let proxy = ProxyHarness::start(&config, reserved);
        let mut client = tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"http/1.1"])
            .await
            .expect("trusted-upstream downstream connection");
        let response = h1_request(&mut client, "/trusted", true)
            .await
            .expect("trusted upstream response");
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"verified-upstream");
        trusted.observations.wait_for_http_requests(1).await;
        assert_eq!(
            trusted.observations.server_names(),
            vec![ORIGIN_SERVER_NAME.to_owned()]
        );
        assert_eq!(trusted.observations.http_requests(), 1);
        assert!(trusted.observations.http_bytes() > 0);
        drop(client);
        proxy.finish().await;
        trusted.finish().await;

        let wrong_name = TlsOrigin::start_h1(b"must-not-be-read").await;
        let reserved = ReservedListener::new();
        let config = proxy_config(
            reserved.address,
            wrong_name.address,
            vec![AlpnProtocol::Http11],
            Some(verified_upstream("wrong.example.test", "ca-a.pem")),
            HttpVersionPolicy::default(),
        );
        let proxy = ProxyHarness::start(&config, reserved);
        let mut client = tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"http/1.1"])
            .await
            .expect("wrong-name downstream connection");
        let response = h1_request(&mut client, "/wrong-name", true)
            .await
            .expect("wrong-name proxy response");
        assert_eq!(response.status, 502);
        wrong_name
            .observations
            .wait_for_completed_handshakes(1)
            .await;
        assert_eq!(wrong_name.observations.accepted(), 1);
        assert_eq!(wrong_name.observations.http_bytes(), 0);
        assert_eq!(wrong_name.observations.http_requests(), 0);
        drop(client);
        proxy.finish().await;
        wrong_name.finish().await;

        let untrusted = TlsOrigin::start_h1(b"must-not-be-read").await;
        let reserved = ReservedListener::new();
        let config = proxy_config(
            reserved.address,
            untrusted.address,
            vec![AlpnProtocol::Http11],
            Some(verified_upstream(ORIGIN_SERVER_NAME, "ca-b.pem")),
            HttpVersionPolicy::default(),
        );
        let proxy = ProxyHarness::start(&config, reserved);
        let mut client = tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"http/1.1"])
            .await
            .expect("untrusted-CA downstream connection");
        let response = h1_request(&mut client, "/untrusted", true)
            .await
            .expect("untrusted-CA proxy response");
        assert_eq!(response.status, 502);
        untrusted
            .observations
            .wait_for_completed_handshakes(1)
            .await;
        assert_eq!(untrusted.observations.accepted(), 1);
        assert_eq!(untrusted.observations.http_bytes(), 0);
        assert_eq!(untrusted.observations.http_requests(), 0);
        drop(client);
        proxy.finish().await;
        untrusted.finish().await;
    })
    .await
    .expect("upstream TLS verification wire test timed out");
}

#[tokio::test]
async fn upstream_tls_without_a_custom_ca_uses_system_roots_and_rejects_private_test_ca() {
    timeout(TEST_TIMEOUT, async {
        let origin = TlsOrigin::start_h1(b"must-not-be-read").await;
        let reserved = ReservedListener::new();
        let config = proxy_config(
            reserved.address,
            origin.address,
            vec![AlpnProtocol::Http11],
            Some(UpstreamTls {
                server_name: ORIGIN_SERVER_NAME.into(),
                ca_certificate_path: None,
            }),
            HttpVersionPolicy::default(),
        );
        let proxy = ProxyHarness::start(&config, reserved);
        let mut client = tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"http/1.1"])
            .await
            .expect("system-root verification downstream connection");

        let response = h1_request(&mut client, "/system-roots", true)
            .await
            .expect("system-root verification proxy response");
        assert_eq!(response.status, 502);
        origin.observations.wait_for_completed_handshakes(1).await;
        assert_eq!(origin.observations.accepted(), 1);
        assert_eq!(origin.observations.http_bytes(), 0);
        assert_eq!(origin.observations.http_requests(), 0);

        drop(client);
        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("upstream system-root verification wire test timed out");
}

#[tokio::test]
async fn upstream_custom_intermediate_ca_is_a_runtime_trust_anchor() {
    timeout(TEST_TIMEOUT, async {
        let chain = generate_test_only_ecdsa_chain(ORIGIN_SERVER_NAME);
        let origin = TlsOrigin::start_h1_with_identity(
            b"partial-chain",
            &chain.fullchain_path,
            &chain.leaf_private_key_path,
        )
        .await;
        let reserved = ReservedListener::new();
        let config = proxy_config(
            reserved.address,
            origin.address,
            vec![AlpnProtocol::Http11],
            Some(UpstreamTls {
                server_name: ORIGIN_SERVER_NAME.into(),
                ca_certificate_path: Some(chain.intermediate_certificate_path.clone()),
            }),
            HttpVersionPolicy::default(),
        );
        let proxy = ProxyHarness::start(&config, reserved);
        let mut client = tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"http/1.1"])
            .await
            .expect("intermediate-anchor downstream connection");

        let response = h1_request(&mut client, "/partial-chain", true)
            .await
            .expect("intermediate-anchor response");
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"partial-chain");
        origin.observations.wait_for_http_requests(1).await;
        assert_eq!(
            origin.observations.server_names(),
            vec![ORIGIN_SERVER_NAME.to_owned()]
        );

        drop(client);
        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("upstream intermediate trust-anchor wire test timed out");
}

#[tokio::test]
async fn upstream_rejects_legacy_tls_before_decrypted_http_and_accepts_modern_tls12() {
    timeout(TEST_TIMEOUT, async {
        for version in [LegacyTlsVersion::Tls10, LegacyTlsVersion::Tls11] {
            let origin = LegacyTlsOrigin::start(version);
            let negotiated = direct_legacy_tls_origin_handshake(origin.address, version)
                .await
                .unwrap_or_else(|error| {
                    panic!("direct {version:?} origin control failed: {error}")
                });
            assert_eq!(negotiated, version.name());
            origin.wait_for_completed_handshakes(1).await;

            let reserved = ReservedListener::new();
            let config = proxy_config(
                reserved.address,
                origin.address,
                vec![AlpnProtocol::Http11],
                Some(verified_upstream(ORIGIN_SERVER_NAME, "ca-a.pem")),
                HttpVersionPolicy::default(),
            );
            let proxy = ProxyHarness::start(&config, reserved);
            let mut client =
                tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"http/1.1"])
                    .await
                    .expect("legacy-origin downstream connection");
            let response = h1_request(&mut client, "/legacy-origin", true)
                .await
                .expect("legacy-origin proxy response");
            assert_eq!(response.status, 502);
            origin.wait_for_completed_handshakes(2).await;
            assert_eq!(origin.accepted(), 2);
            assert_eq!(origin.decrypted_bytes(), 0);

            drop(client);
            proxy.finish().await;
            origin.finish();
        }

        let origin = TlsOrigin::start_h1_tls12(b"modern-tls12").await;
        let reserved = ReservedListener::new();
        let config = proxy_config(
            reserved.address,
            origin.address,
            vec![AlpnProtocol::Http11],
            Some(verified_upstream(ORIGIN_SERVER_NAME, "ca-a.pem")),
            HttpVersionPolicy {
                min: HttpVersion::Http11,
                max: HttpVersion::Http2,
            },
        );
        let proxy = ProxyHarness::start(&config, reserved);
        let mut client = tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"http/1.1"])
            .await
            .expect("TLS1.2-control downstream connection");
        let response = h1_request(&mut client, "/modern-tls12", true)
            .await
            .expect("TLS1.2 modern-suite response");
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"modern-tls12");
        origin.observations.wait_for_http_requests(1).await;
        assert_eq!(origin.observations.tls_versions(), ["TLSv1_2"]);
        assert!(
            origin
                .observations
                .cipher_suites()
                .iter()
                .all(|suite| { suite.contains("GCM") || suite.contains("CHACHA20_POLY1305") })
        );

        drop(client);
        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("upstream TLS policy wire test timed out");
}

#[tokio::test]
async fn h2_only_upstream_preserves_grpc_data_and_trailers_without_h1_downgrade() {
    timeout(TEST_TIMEOUT, async {
        let origin = TlsOrigin::start_h2().await;
        let reserved = ReservedListener::new();
        let h2_only = HttpVersionPolicy {
            min: HttpVersion::Http2,
            max: HttpVersion::Http2,
        };
        let config = proxy_config(
            reserved.address,
            origin.address,
            vec![AlpnProtocol::H2, AlpnProtocol::Http11],
            Some(verified_upstream(ORIGIN_SERVER_NAME, "ca-a.pem")),
            h2_only,
        );
        let proxy = ProxyHarness::start(&config, reserved);
        let stream = tls_connect(
            proxy.address,
            PROXY_SERVER_NAME,
            "ca-a.pem",
            &[b"h2", b"http/1.1"],
        )
        .await
        .expect("H2 downstream connection");
        assert_eq!(negotiated_alpn(&stream), Some(b"h2".as_slice()));
        let mut client = H2Client::from_tls(stream)
            .await
            .expect("H2 client handshake");

        let h2 = client
            .request(Method::GET, "/h2")
            .await
            .expect("H2-only upstream response");
        assert_eq!(h2.status, StatusCode::OK);
        assert_eq!(h2.body, b"h2-origin".as_slice());

        let grpc = client
            .request(Method::POST, "/grpc/full")
            .await
            .expect("full gRPC response");
        assert_eq!(grpc.status, StatusCode::OK);
        assert_eq!(grpc.headers["content-type"], "application/grpc");
        assert_eq!(grpc.body, GRPC_BODY);
        let trailers = grpc.trailers.expect("full gRPC trailers");
        assert_eq!(trailers["grpc-status"], "0");
        assert_eq!(trailers["grpc-message"], "completed");
        assert_eq!(trailers["x-oxiroute-trailer"], "full");

        let trailers_only = client
            .request(Method::POST, "/grpc/error")
            .await
            .expect("trailers-only gRPC response");
        assert_eq!(trailers_only.status, StatusCode::OK);
        assert_eq!(trailers_only.headers["content-type"], "application/grpc");
        assert!(trailers_only.body.is_empty());
        let trailers = trailers_only.trailers.expect("trailers-only metadata");
        assert_eq!(trailers["grpc-status"], "7");
        assert_eq!(trailers["grpc-message"], "permission denied");
        assert_eq!(trailers["x-oxiroute-trailer"], "trailers-only");

        origin.observations.wait_for_http_requests(3).await;
        assert_eq!(origin.observations.http_requests(), 3);
        assert_eq!(
            origin.observations.server_names(),
            vec![ORIGIN_SERVER_NAME.to_owned()]
        );
        assert_eq!(origin.observations.alpn(), vec![b"h2".to_vec()]);

        client.finish().await;
        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("upstream H2/gRPC wire test timed out");
}

#[tokio::test]
async fn maxconn_one_multiplexes_concurrent_h2_requests_on_one_physical_connection() {
    timeout(TEST_TIMEOUT, async {
        let origin = TlsOrigin::start_h2().await;
        let reserved = ReservedListener::new();
        let mut config = proxy_config(
            reserved.address,
            origin.address,
            vec![AlpnProtocol::H2],
            Some(verified_upstream(ORIGIN_SERVER_NAME, "ca-a.pem")),
            HttpVersionPolicy {
                min: HttpVersion::Http2,
                max: HttpVersion::Http2,
            },
        );
        let endpoint = config.upstream_pools[0]
            .endpoints
            .pop()
            .expect("upstream endpoint");
        config.upstream_pools[0].servers.push(UpstreamServer {
            name: "origin".into(),
            endpoint,
            max_connections: Some(1),
            dns_resolution: oxiroute_config::DnsResolutionPolicy::OnConnect,
        });
        config.upstream_pools[0].queue_timeout_ms = Some(1_000);
        let proxy = ProxyHarness::start(&config, reserved);
        let proxy_address = proxy.address;
        let request = |path: &'static str| async move {
            let stream = tls_connect(proxy_address, PROXY_SERVER_NAME, "ca-a.pem", &[b"h2"])
                .await
                .expect("capped downstream H2 connection");
            let mut client = H2Client::from_tls(stream)
                .await
                .expect("capped downstream H2 client");
            let response = client
                .request(Method::GET, path)
                .await
                .expect("capped H2 response");
            client.finish().await;
            response
        };

        let (first, second) = tokio::join!(request("/h2"), request("/h2"));
        assert_eq!(first.body.as_ref(), b"h2-origin");
        assert_eq!(second.body.as_ref(), b"h2-origin");
        origin.observations.wait_for_http_requests(2).await;
        assert_eq!(origin.observations.alpn(), vec![b"h2".to_vec()]);

        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("capped H2 multiplexing test timed out");
}

#[tokio::test]
async fn h2_refused_stream_retries_a_bodyless_get_on_a_distinct_endpoint() {
    timeout(TEST_TIMEOUT, async {
        let refusing = TlsOrigin::start_h2_refusing_streams().await;
        let healthy = TlsOrigin::start_h2().await;
        let reserved = ReservedListener::new();
        let h2_only = HttpVersionPolicy {
            min: HttpVersion::Http2,
            max: HttpVersion::Http2,
        };
        let mut config = proxy_config(
            reserved.address,
            refusing.address,
            vec![AlpnProtocol::H2],
            Some(verified_upstream(ORIGIN_SERVER_NAME, "ca-a.pem")),
            h2_only,
        );
        config.upstream_pools[0]
            .endpoints
            .push(socket_endpoint(healthy.address));
        let HttpRouteAction::Proxy { policy, .. } = &mut config.http_services[0].routes[0].action
        else {
            panic!("wire route must proxy");
        };
        policy.retry = HttpRetryPolicy {
            max_retries: 1,
            triggers: vec![HttpRetryTrigger::RefusedStream],
            ..HttpRetryPolicy::default()
        };
        let proxy = ProxyHarness::start(&config, reserved);
        let stream = tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"h2"])
            .await
            .expect("downstream H2 retry TLS connect");
        let mut client = H2Client::from_tls(stream)
            .await
            .expect("downstream H2 retry client");

        let response = client
            .request(Method::GET, "/h2")
            .await
            .expect("retried H2 request");
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.body, b"h2-origin".as_slice());
        refusing.observations.wait_for_http_requests(1).await;
        healthy.observations.wait_for_http_requests(1).await;

        client.finish().await;
        proxy.finish().await;
        refusing.finish().await;
        healthy.finish().await;
    })
    .await
    .expect("H2 refused-stream retry test timed out");
}

#[tokio::test]
async fn h2_only_upstream_without_compatible_alpn_fails_before_http_headers() {
    timeout(TEST_TIMEOUT, async {
        let origin = TlsOrigin::start_h1(b"must-not-be-read").await;
        let reserved = ReservedListener::new();
        let h2_only = HttpVersionPolicy {
            min: HttpVersion::Http2,
            max: HttpVersion::Http2,
        };
        let config = proxy_config(
            reserved.address,
            origin.address,
            vec![AlpnProtocol::Http11],
            Some(verified_upstream(ORIGIN_SERVER_NAME, "ca-a.pem")),
            h2_only,
        );
        let proxy = ProxyHarness::start(&config, reserved);
        let mut client = tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"http/1.1"])
            .await
            .expect("H2-only mismatch downstream connection");

        let response = h1_request(&mut client, "/h2-alpn-mismatch", true)
            .await
            .expect("H2-only mismatch proxy response");
        assert_eq!(response.status, 502);
        origin.observations.wait_for_completed_handshakes(1).await;
        assert_eq!(origin.observations.accepted(), 1);
        assert_eq!(origin.observations.http_bytes(), 0);
        assert_eq!(origin.observations.http_requests(), 0);
        assert!(origin.observations.alpn().is_empty());

        drop(client);
        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("upstream H2-only ALPN mismatch wire test timed out");
}

#[tokio::test]
async fn flexible_upstream_versions_fall_back_to_http_1_1_alpn() {
    timeout(TEST_TIMEOUT, async {
        let origin = TlsOrigin::start_h1(b"h1-fallback").await;
        let reserved = ReservedListener::new();
        let config = proxy_config(
            reserved.address,
            origin.address,
            vec![AlpnProtocol::Http11],
            Some(verified_upstream(ORIGIN_SERVER_NAME, "ca-a.pem")),
            HttpVersionPolicy {
                min: HttpVersion::Http11,
                max: HttpVersion::Http2,
            },
        );
        let proxy = ProxyHarness::start(&config, reserved);
        let mut client = tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"http/1.1"])
            .await
            .expect("flexible-version downstream connection");

        let response = h1_request(&mut client, "/h1-fallback", true)
            .await
            .expect("flexible-version upstream response");
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"h1-fallback");
        origin.observations.wait_for_http_requests(1).await;
        assert_eq!(origin.observations.http_requests(), 1);
        assert!(origin.observations.http_bytes() > 0);
        assert_eq!(origin.observations.alpn(), vec![b"http/1.1".to_vec()]);

        drop(client);
        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("upstream flexible-version H1 fallback wire test timed out");
}

#[tokio::test]
async fn certificate_rotation_changes_new_handshakes_without_mutating_existing_connections() {
    timeout(TEST_TIMEOUT, async {
        let origin = PlainH1Origin::start(b"rotation").await;
        let reserved = ReservedListener::new();
        let config = proxy_config(
            reserved.address,
            origin.address,
            vec![AlpnProtocol::Http11],
            None,
            HttpVersionPolicy::default(),
        );
        let proxy = ProxyHarness::start(&config, reserved);
        let initial_leaf = fixture_leaf("proxy-a.pem");
        let replacement_leaf = fixture_leaf("proxy-b.pem");
        let client_config = tls_client_config(&fixture("ca-a.pem"), &[b"http/1.1"])
            .expect("shared rotation rustls client config");

        let mut existing =
            tls_connect_with_config(proxy.address, PROXY_SERVER_NAME, Arc::clone(&client_config))
                .await
                .expect("existing TLS connection");
        assert_eq!(handshake_kind(&existing), Some(HandshakeKind::Full));
        assert_eq!(peer_leaf(&existing), initial_leaf);
        let first = h1_request(&mut existing, "/before-rotation", false)
            .await
            .expect("pre-rotation response");
        assert_eq!(first.body, b"rotation");

        let expected = proxy.active_certificate.snapshot();
        let different_san_key = private_key_fixture("origin-key.pem");
        let different_san_chain = certificate_chain_fixture("origin.pem", "ca-a.pem");
        let different_sans = Arc::new(
            CertificateGeneration::from_files(
                "downstream",
                &[ORIGIN_SERVER_NAME.into()],
                different_san_chain.path(),
                different_san_key.path(),
            )
            .expect("different-SAN certificate generation"),
        );
        let error = proxy
            .active_certificate
            .publish_if_current(&different_sans, Arc::clone(&different_sans))
            .expect_err("same-name replacement must retain the bound SAN set");
        assert!(matches!(
            error,
            oxiroute_server::CertificatePublishError::DnsNamesMismatch { .. }
        ));
        assert!(Arc::ptr_eq(&proxy.active_certificate.snapshot(), &expected));

        let replacement_key = private_key_fixture("proxy-b-key.pem");
        let replacement = Arc::new(
            CertificateGeneration::from_files(
                "downstream",
                &[PROXY_SERVER_NAME.into()],
                &fixture("proxy-b.pem"),
                replacement_key.path(),
            )
            .expect("replacement certificate generation"),
        );
        proxy
            .active_certificate
            .publish_if_current(&expected, replacement)
            .expect("publish replacement certificate");

        let second = h1_request(&mut existing, "/existing-after-rotation", false)
            .await
            .expect("existing post-rotation response");
        assert_eq!(second.body, b"rotation");
        assert_eq!(peer_leaf(&existing), initial_leaf);

        let mut fresh =
            tls_connect_with_config(proxy.address, PROXY_SERVER_NAME, Arc::clone(&client_config))
                .await
                .expect("fresh TLS connection using shared session cache");
        assert_eq!(
            handshake_kind(&fresh),
            Some(HandshakeKind::Full),
            "a resumed session would retain the old authenticated identity"
        );
        assert_eq!(peer_leaf(&fresh), replacement_leaf);
        let third = h1_request(&mut fresh, "/new-after-rotation", true)
            .await
            .expect("new post-rotation response");
        assert_eq!(third.body, b"rotation");
        origin.wait_for_requests(3).await;

        drop(existing);
        drop(fresh);
        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("certificate rotation wire test timed out");
}

#[tokio::test]
async fn rotating_one_sni_identity_does_not_change_other_certificate_generations() {
    timeout(TEST_TIMEOUT, async {
        let initial_tenant = generate_test_only_ecdsa_chain(TENANT_SNI_SERVER_NAME);
        let replacement_tenant = generate_test_only_ecdsa_chain(TENANT_SNI_SERVER_NAME);
        let origin = PlainH1Origin::start(b"per-identity-rotation").await;
        let reserved = ReservedListener::new();
        let mut config = proxy_config(
            reserved.address,
            origin.address,
            vec![AlpnProtocol::Http11],
            None,
            HttpVersionPolicy::default(),
        );
        config.certificates.push(Certificate {
            name: "tenant".into(),
            dns_names: vec![TENANT_SNI_SERVER_NAME.into()],
            source: CertificateSource::Files {
                certificate_chain_path: initial_tenant.fullchain_path.clone(),
                private_key_path: initial_tenant.leaf_private_key_path.clone(),
            },
        });
        config.tls_profiles[0].certificates.push("tenant".into());
        let proxy = ProxyHarness::start(&config, reserved);

        let initial_client =
            tls_client_config(&initial_tenant.root_certificate_path, &[b"http/1.1"])
                .expect("initial tenant rustls client config");
        let mut existing_tenant =
            tls_connect_with_config(proxy.address, TENANT_SNI_SERVER_NAME, initial_client)
                .await
                .expect("initial tenant connection");
        assert_eq!(peer_leaf(&existing_tenant), initial_tenant.leaf_der);
        assert_eq!(
            h1_request(&mut existing_tenant, "/tenant-before", false)
                .await
                .expect("tenant request before rotation")
                .body,
            b"per-identity-rotation"
        );

        let default_before = proxy.active_certificate.snapshot();
        let tenant = proxy.certificate("tenant");
        let tenant_before = tenant.snapshot();
        let replacement = Arc::new(
            CertificateGeneration::from_files(
                "tenant",
                &[TENANT_SNI_SERVER_NAME.into()],
                &replacement_tenant.fullchain_path,
                &replacement_tenant.leaf_private_key_path,
            )
            .expect("replacement tenant certificate generation"),
        );
        tenant
            .publish_if_current(&tenant_before, replacement)
            .expect("publish tenant certificate replacement");

        assert!(Arc::ptr_eq(
            &proxy.active_certificate.snapshot(),
            &default_before
        ));
        assert_eq!(peer_leaf(&existing_tenant), initial_tenant.leaf_der);
        assert_eq!(
            h1_request(&mut existing_tenant, "/tenant-existing", true)
                .await
                .expect("existing tenant request after rotation")
                .body,
            b"per-identity-rotation"
        );

        let replacement_client =
            tls_client_config(&replacement_tenant.root_certificate_path, &[b"http/1.1"])
                .expect("replacement tenant rustls client config");
        let mut fresh_tenant =
            tls_connect_with_config(proxy.address, TENANT_SNI_SERVER_NAME, replacement_client)
                .await
                .expect("replacement tenant connection");
        assert_eq!(peer_leaf(&fresh_tenant), replacement_tenant.leaf_der);
        assert_eq!(
            h1_request(&mut fresh_tenant, "/tenant-after", true)
                .await
                .expect("tenant request after rotation")
                .body,
            b"per-identity-rotation"
        );

        let mut fresh_default =
            tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"http/1.1"])
                .await
                .expect("default identity after tenant rotation");
        assert_eq!(peer_leaf(&fresh_default), fixture_leaf("proxy-a.pem"));
        assert_eq!(
            h1_request(&mut fresh_default, "/default-after", true)
                .await
                .expect("default request after tenant rotation")
                .body,
            b"per-identity-rotation"
        );
        origin.wait_for_requests(4).await;

        drop(existing_tenant);
        drop(fresh_tenant);
        drop(fresh_default);
        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("per-identity certificate rotation wire test timed out");
}

#[tokio::test]
async fn certbot_watcher_rotates_new_handshakes_without_changing_existing_connections_or_sni_peers()
{
    timeout(TEST_TIMEOUT, async {
        let initial_tenant = generate_test_only_ecdsa_chain(TENANT_SNI_SERVER_NAME);
        let replacement_tenant = generate_test_only_ecdsa_chain(TENANT_SNI_SERVER_NAME);
        let lineage = TestCertbotLineage::new("tenant", &initial_tenant);
        lineage.write_revision(2, &replacement_tenant);
        let origin = PlainH1Origin::start(b"certbot-rotation").await;
        let reserved = ReservedListener::new();
        let mut config = proxy_config(
            reserved.address,
            origin.address,
            vec![AlpnProtocol::Http11],
            None,
            HttpVersionPolicy::default(),
        );
        config.certificates.push(Certificate {
            name: "tenant".into(),
            dns_names: vec![TENANT_SNI_SERVER_NAME.into()],
            source: lineage.source(),
        });
        config.tls_profiles[0].certificates.push("tenant".into());
        let proxy = ProxyHarness::start(&config, reserved);

        let initial_client =
            tls_client_config(&initial_tenant.root_certificate_path, &[b"http/1.1"])
                .expect("initial Certbot tenant client config");
        let mut existing_tenant =
            tls_connect_with_config(proxy.address, TENANT_SNI_SERVER_NAME, initial_client)
                .await
                .expect("initial Certbot tenant connection");
        assert_eq!(peer_leaf(&existing_tenant), initial_tenant.leaf_der);
        assert_eq!(
            h1_request(&mut existing_tenant, "/certbot-before", false)
                .await
                .expect("request before Certbot rotation")
                .body,
            b"certbot-rotation"
        );
        assert_eq!(
            proxy.certbot_reconciler("tenant").active_archive_revision(),
            1
        );

        lineage.activate(2);
        timeout(TEST_TIMEOUT, async {
            while proxy.certbot_reconciler("tenant").active_archive_revision() != 2 {
                sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("Certbot watcher did not activate revision 2");
        assert_eq!(
            proxy.certbot_reconciler("tenant").status().last_outcome,
            Some("activated_forward")
        );

        assert_eq!(peer_leaf(&existing_tenant), initial_tenant.leaf_der);
        assert_eq!(
            h1_request(&mut existing_tenant, "/certbot-existing", true)
                .await
                .expect("existing request after Certbot rotation")
                .body,
            b"certbot-rotation"
        );

        let replacement_client =
            tls_client_config(&replacement_tenant.root_certificate_path, &[b"http/1.1"])
                .expect("replacement Certbot tenant client config");
        let mut fresh_tenant =
            tls_connect_with_config(proxy.address, TENANT_SNI_SERVER_NAME, replacement_client)
                .await
                .expect("replacement Certbot tenant connection");
        assert_eq!(peer_leaf(&fresh_tenant), replacement_tenant.leaf_der);
        assert_eq!(
            h1_request(&mut fresh_tenant, "/certbot-after", true)
                .await
                .expect("fresh request after Certbot rotation")
                .body,
            b"certbot-rotation"
        );

        let mut fresh_default =
            tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"http/1.1"])
                .await
                .expect("default identity after Certbot rotation");
        assert_eq!(peer_leaf(&fresh_default), fixture_leaf("proxy-a.pem"));
        assert_eq!(
            h1_request(&mut fresh_default, "/default-after-certbot", true)
                .await
                .expect("default request after Certbot rotation")
                .body,
            b"certbot-rotation"
        );
        origin.wait_for_requests(4).await;

        drop(existing_tenant);
        drop(fresh_tenant);
        drop(fresh_default);
        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("Certbot watcher wire rotation test timed out");
}

#[tokio::test]
async fn concurrent_handshake_waves_switch_complete_generation_after_publication() {
    const HANDSHAKES_PER_WAVE: usize = 16;

    timeout(TEST_TIMEOUT, async {
        let origin = PlainH1Origin::start(b"unused").await;
        let reserved = ReservedListener::new();
        let config = proxy_config(
            reserved.address,
            origin.address,
            vec![AlpnProtocol::Http11],
            None,
            HttpVersionPolicy::default(),
        );
        let proxy = ProxyHarness::start(&config, reserved);
        let initial_leaf = fixture_leaf("proxy-a.pem");
        let replacement_leaf = fixture_leaf("proxy-b.pem");
        let client_config = tls_client_config(&fixture("ca-a.pem"), &[b"http/1.1"])
            .expect("concurrent rustls client config");
        let initial_wave = concurrent_peer_leaves(
            proxy.address,
            Arc::clone(&client_config),
            HANDSHAKES_PER_WAVE,
        )
        .await;
        assert!(initial_wave.iter().all(|leaf| leaf == &initial_leaf));

        let expected = proxy.active_certificate.snapshot();
        let replacement_key = private_key_fixture("proxy-b-key.pem");
        let replacement = Arc::new(
            CertificateGeneration::from_files(
                "downstream",
                &[PROXY_SERVER_NAME.into()],
                &fixture("proxy-b.pem"),
                replacement_key.path(),
            )
            .expect("concurrent replacement certificate generation"),
        );
        proxy
            .active_certificate
            .publish_if_current(&expected, replacement)
            .expect("publish between concurrent handshake waves");
        let replacement_wave = concurrent_peer_leaves(
            proxy.address,
            Arc::clone(&client_config),
            HANDSHAKES_PER_WAVE,
        )
        .await;
        assert!(
            replacement_wave
                .iter()
                .all(|leaf| leaf == &replacement_leaf)
        );

        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("concurrent certificate generation wire test timed out");
}

async fn concurrent_peer_leaves(
    address: SocketAddr,
    client_config: Arc<rustls::ClientConfig>,
    count: usize,
) -> Vec<Vec<u8>> {
    let mut handshakes = JoinSet::new();
    for _ in 0..count {
        let client_config = Arc::clone(&client_config);
        handshakes.spawn(async move {
            let stream = tls_connect_with_config(address, PROXY_SERVER_NAME, client_config).await?;
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(peer_leaf(&stream))
        });
    }

    let mut observed = Vec::with_capacity(count);
    while let Some(result) = handshakes.join_next().await {
        observed.push(
            result
                .expect("concurrent handshake task")
                .expect("concurrent TLS handshake"),
        );
    }
    assert_eq!(observed.len(), count);
    observed
}
