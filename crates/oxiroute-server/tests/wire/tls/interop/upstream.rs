use oxiroute_config::{AlpnProtocol, HttpVersion, HttpVersionPolicy, UpstreamTls};
use tokio::time::timeout;

use crate::support::{
    LegacyTlsOrigin, LegacyTlsVersion, ORIGIN_SERVER_NAME, PROXY_SERVER_NAME, ProxyHarness,
    ReservedListener, TEST_TIMEOUT, TlsOrigin, direct_legacy_tls_origin_handshake,
    generate_test_only_ecdsa_chain, h1_request, proxy_config, tls_connect, verified_upstream,
};

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
        let proxy = ProxyHarness::start(&config.validate(), reserved);
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
        let proxy = ProxyHarness::start(&config.validate(), reserved);
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
        let proxy = ProxyHarness::start(&config.validate(), reserved);
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
                server_name: Some(ORIGIN_SERVER_NAME.into()),
                verify: oxiroute_config::UpstreamTlsVerify::Full,
                ca_certificate_path: None,
            }),
            HttpVersionPolicy::default(),
        );
        let proxy = ProxyHarness::start(&config.validate(), reserved);
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

async fn proxy_through_unverified_pool(
    server_name: Option<&str>,
    verify: oxiroute_config::UpstreamTlsVerify,
    path: &str,
) -> (u16, Vec<u8>, Vec<String>, usize) {
    let origin = TlsOrigin::start_h1(b"unverified-origin").await;
    let reserved = ReservedListener::new();
    let config = proxy_config(
        reserved.address,
        origin.address,
        vec![AlpnProtocol::Http11],
        Some(UpstreamTls {
            server_name: server_name.map(Into::into),
            verify,
            ca_certificate_path: None,
        }),
        HttpVersionPolicy::default(),
    );
    let proxy = ProxyHarness::start(&config.validate(), reserved);
    let mut client = tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"http/1.1"])
        .await
        .expect("downstream connection");
    let response = h1_request(&mut client, path, true)
        .await
        .expect("proxy response");
    origin.observations.wait_for_completed_handshakes(1).await;
    let observed = (
        response.status,
        response.body.clone(),
        origin.observations.server_names(),
        origin.observations.http_requests(),
    );
    drop(client);
    proxy.finish().await;
    origin.finish().await;
    observed
}

#[tokio::test]
async fn upstream_tls_verify_none_reaches_an_untrusted_origin_without_sni() {
    use oxiroute_config::UpstreamTlsVerify;

    timeout(TEST_TIMEOUT, async {
        let (status, body, server_names, requests) =
            proxy_through_unverified_pool(None, UpstreamTlsVerify::None, "/unverified").await;
        assert_eq!(status, 200);
        assert_eq!(body, b"unverified-origin");
        assert!(
            server_names.is_empty(),
            "no SNI on the wire: {server_names:?}"
        );
        assert_eq!(requests, 1);

        let (status, _, server_names, requests) = proxy_through_unverified_pool(
            Some("wrong.example.test"),
            UpstreamTlsVerify::None,
            "/unverified-sni",
        )
        .await;
        assert_eq!(
            status, 200,
            "the name is sent but not matched against the certificate"
        );
        assert_eq!(server_names, vec!["wrong.example.test".to_owned()]);
        assert_eq!(requests, 1);

        let (status, _, _, requests) = proxy_through_unverified_pool(
            Some(ORIGIN_SERVER_NAME),
            UpstreamTlsVerify::Full,
            "/verified",
        )
        .await;
        assert_eq!(
            status, 502,
            "the same origin is refused when the pool verifies"
        );
        assert_eq!(requests, 0);
    })
    .await
    .expect("unverified upstream TLS wire test timed out");
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
                server_name: Some(ORIGIN_SERVER_NAME.into()),
                verify: oxiroute_config::UpstreamTlsVerify::Full,
                ca_certificate_path: Some(chain.intermediate_certificate_path.clone()),
            }),
            HttpVersionPolicy::default(),
        );
        let proxy = ProxyHarness::start(&config.validate(), reserved);
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
            let proxy = ProxyHarness::start(&config.validate(), reserved);
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
        let proxy = ProxyHarness::start(&config.validate(), reserved);
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
        let proxy = ProxyHarness::start(&config.validate(), reserved);
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
        let proxy = ProxyHarness::start(&config.validate(), reserved);
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
