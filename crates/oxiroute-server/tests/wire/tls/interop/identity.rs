use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use oxiroute_config::{AlpnProtocol, Certificate, CertificateSource, HttpVersionPolicy};
use oxiroute_server::TlsAlpnChallenge;
use tokio::time::timeout;

use crate::support::{
    PROXY_SERVER_NAME, PlainH1Origin, ProxyHarness, ReservedListener, TEST_TIMEOUT, fixture,
    fixture_leaf, generate_test_only_ecdsa_chain, h1_request, openssl_tls_alpn_handshake,
    peer_certificate_count, peer_leaf, proxy_config, tls_client_config, tls_connect,
    tls_connect_with_config,
};

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
        let proxy = ProxyHarness::start(&config.validate(), reserved);
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
        let proxy = ProxyHarness::start(&config.validate(), reserved);

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
#[allow(clippy::too_many_lines)]
async fn downstream_tls_alpn01_selects_only_owned_live_challenges() {
    timeout(TEST_TIMEOUT, async {
        let origin = PlainH1Origin::start(b"tls-alpn01").await;
        let reserved = ReservedListener::new();
        let config = proxy_config(
            reserved.address,
            origin.address,
            vec![AlpnProtocol::Http11],
            None,
            HttpVersionPolicy::default(),
        );
        let proxy = ProxyHarness::start(&config.validate(), reserved);
        let mut ordinary =
            tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"http/1.1"])
                .await
                .expect("ordinary TLS connection before challenge");
        let ordinary_leaf = peer_leaf(&ordinary);

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_secs();
        let lease = proxy
            .tls_alpn_challenges
            .provision(
                TlsAlpnChallenge::generate(
                    PROXY_SERVER_NAME,
                    "token.thumbprint",
                    "account-1",
                    "order-1",
                    "authorization-1",
                    "challenge-1",
                    now,
                    now + 300,
                )
                .expect("TLS-ALPN-01 challenge identity"),
            )
            .expect("provision TLS-ALPN-01 challenge");
        assert!(
            proxy
                .tls_alpn_challenges
                .provision(
                    TlsAlpnChallenge::generate(
                        PROXY_SERVER_NAME,
                        "other.thumbprint",
                        "account-2",
                        "order-2",
                        "authorization-2",
                        "challenge-2",
                        now,
                        now + 300,
                    )
                    .expect("second TLS-ALPN-01 identity"),
                )
                .is_err()
        );
        let challenge_leaf = lease
            .identity()
            .certificate()
            .to_der()
            .expect("challenge certificate DER");
        let challenge_connection =
            openssl_tls_alpn_handshake(proxy.address, PROXY_SERVER_NAME, &[b"acme-tls/1"])
                .await
                .expect("TLS-ALPN-01 handshake");
        assert_eq!(
            challenge_connection.negotiated_alpn.as_deref(),
            Some(b"acme-tls/1".as_slice())
        );
        assert_eq!(challenge_connection.peer_leaf, challenge_leaf);

        let mut wrong_alpn =
            tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"http/1.1"])
                .await
                .expect("ordinary ALPN remains on the normal certificate");
        assert_eq!(peer_leaf(&wrong_alpn), ordinary_leaf);
        assert!(
            openssl_tls_alpn_handshake(proxy.address, "wrong.example.test", &[b"acme-tls/1"],)
                .await
                .is_err()
        );

        lease.complete();
        let expired = proxy
            .tls_alpn_challenges
            .provision(
                TlsAlpnChallenge::generate(
                    PROXY_SERVER_NAME,
                    "expired.thumbprint",
                    "account-3",
                    "order-3",
                    "authorization-3",
                    "challenge-3",
                    now.saturating_sub(2),
                    now.saturating_sub(1),
                )
                .expect("expired TLS-ALPN-01 identity"),
            )
            .expect("provision expired identity for expiry check");
        assert!(
            openssl_tls_alpn_handshake(proxy.address, PROXY_SERVER_NAME, &[b"acme-tls/1"],)
                .await
                .is_err()
        );
        expired.cancel();

        let response = h1_request(&mut ordinary, "/ordinary-after-challenge", true)
            .await
            .expect("existing ordinary connection after challenge");
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"tls-alpn01");
        assert!(
            h1_request(&mut wrong_alpn, "/ordinary-alpn", true)
                .await
                .is_ok()
        );
        origin.wait_for_requests(2).await;

        drop(ordinary);
        drop(wrong_alpn);
        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("TLS-ALPN-01 downstream selection wire test timed out");
}
