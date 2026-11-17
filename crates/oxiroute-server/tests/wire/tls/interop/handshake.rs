use http::{Method, StatusCode};
use oxiroute_config::{
    AlpnProtocol, HttpRequestHeaderMutation, HttpRequestHeaderValue, HttpRouteAction,
    HttpVersionPolicy, TlsVersion,
};
use rustls::ProtocolVersion;
use tokio::time::{sleep, timeout};

use crate::support::{
    self, H2Client, LegacyTlsVersion, ORIGIN_SERVER_NAME, PROXY_SERVER_NAME, PlainH1Origin,
    ProxyHarness, ReservedListener, TEST_TIMEOUT, TlsOrigin, fixture, h1_request,
    legacy_tls_handshake, negotiated_alpn, negotiated_tls_is_modern, proxy_config, tcp_connect,
    tls_client_config_with_versions, tls_connect, tls_connect_with_config, verified_upstream,
};

fn rustls_backend() -> bool {
    support::test_tls_policy().backend.unwrap_or_default() == oxiroute_config::TlsBackend::Rustls
}

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
        let proxy = ProxyHarness::start(&config.validate(), reserved);

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
        let proxy = ProxyHarness::start(&config.validate(), reserved);
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
        let proxy = ProxyHarness::start(&config.validate(), reserved);

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
        let proxy = ProxyHarness::start(&config.validate(), reserved);

        for version in [LegacyTlsVersion::Tls10, LegacyTlsVersion::Tls11] {
            let rejected = legacy_tls_handshake(proxy.address, version)
                .await
                .unwrap_or_else(|error| panic!("{version:?} client attempt failed: {error}"));
            // rustls answers a client that offers nothing newer than TLS 1.1 with handshake_failure, OpenSSL with
            // protocol_version.
            let alert = if rustls_backend() {
                "handshake failure"
            } else {
                "protocol version"
            };
            assert!(
                rejected.error.contains(alert),
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
        let proxy = ProxyHarness::start(&config.validate(), reserved);

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
        let proxy = ProxyHarness::start(&config.validate(), reserved);

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
