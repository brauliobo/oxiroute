use std::{net::SocketAddr, sync::Arc};

use oxiroute_config::{AlpnProtocol, Certificate, CertificateSource, HttpVersionPolicy};
use oxiroute_server::CertificateGeneration;
use rustls::HandshakeKind;
use tokio::{
    task::JoinSet,
    time::{sleep, timeout},
};

use crate::support::{
    ORIGIN_SERVER_NAME, PROXY_SERVER_NAME, PlainH1Origin, ProxyHarness, ReservedListener,
    TEST_TIMEOUT, TestCertbotLineage, certificate_chain_fixture, fixture, fixture_leaf,
    generate_test_only_ecdsa_chain, h1_request, handshake_kind, peer_leaf, private_key_fixture,
    proxy_config, tls_client_config, tls_connect, tls_connect_with_config,
};

const TENANT_SNI_SERVER_NAME: &str = "tenant.sni.example.test";

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
        let proxy = ProxyHarness::start(&config.validate(), reserved);
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
        let proxy = ProxyHarness::start(&config.validate(), reserved);

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
        let proxy = ProxyHarness::start(&config.validate(), reserved);

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
        let proxy = ProxyHarness::start(&config.validate(), reserved);
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
