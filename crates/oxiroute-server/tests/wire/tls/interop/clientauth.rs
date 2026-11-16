use std::fs;

use http::{Method, StatusCode};
use oxiroute_config::{AlpnProtocol, HttpVersionPolicy, TlsClientAuthMode, TlsClientAuthPolicy};
use tokio::time::timeout;

use crate::support::{
    H2Client, PROXY_SERVER_NAME, PlainH1Origin, ProxyHarness, ReservedListener, TEST_TIMEOUT,
    fixture, generate_test_only_client_chain, h1_request, negotiated_alpn,
    openssl_tls_request_with_identity, proxy_config, tls_client_config_with_identity, tls_connect,
    tls_connect_with_config,
};

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
        let proxy = ProxyHarness::start(&config.validate(), reserved);

        let no_certificate_rejected =
            match tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"http/1.1"]).await {
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
        let proxy = ProxyHarness::start(&config.validate(), reserved);

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
        let wrong_san_rejected =
            match tls_connect_with_config(proxy.address, PROXY_SERVER_NAME, wrong_san_config).await
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
async fn required_downstream_client_auth_preserves_h2_alpn_and_grpc_transport() {
    timeout(TEST_TIMEOUT, async {
        let client_identity = generate_test_only_client_chain("client.example.test");
        let origin = PlainH1Origin::start(b"h2-client-auth").await;
        let reserved = ReservedListener::new();
        let mut config = proxy_config(
            reserved.address,
            origin.address,
            vec![AlpnProtocol::H2],
            None,
            HttpVersionPolicy::default(),
        );
        config.tls_profiles[0].policy.client_auth = TlsClientAuthPolicy {
            mode: TlsClientAuthMode::Required,
            ca_certificate_path: Some(client_identity.root_certificate_path.clone()),
            allowed_dns_names: vec!["client.example.test".into()],
        };
        let proxy = ProxyHarness::start(&config.validate(), reserved);

        let no_certificate_rejected =
            match tls_connect(proxy.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"h2"]).await {
                Ok(stream) => match H2Client::from_tls(stream).await {
                    Ok(mut client) => client
                        .request(Method::POST, "/h2-client-auth")
                        .await
                        .is_err(),
                    Err(_) => true,
                },
                Err(_) => true,
            };
        assert!(
            no_certificate_rejected,
            "required H2 client auth accepted no certificate"
        );
        assert_eq!(origin.requests(), 0);

        let client_config = tls_client_config_with_identity(
            &fixture("ca-a.pem"),
            &client_identity.fullchain_path,
            &client_identity.leaf_private_key_path,
            &[b"h2"],
        )
        .expect("H2 client certificate config");
        let stream = tls_connect_with_config(proxy.address, PROXY_SERVER_NAME, client_config)
            .await
            .expect("required H2 client auth connection");
        assert_eq!(negotiated_alpn(&stream), Some(b"h2".as_slice()));
        let mut client = H2Client::from_tls(stream)
            .await
            .expect("required H2 client auth client");
        let response = client
            .request(Method::POST, "/h2-client-auth")
            .await
            .expect("required H2 client auth response");
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.body.as_ref(), b"h2-client-auth");
        origin.wait_for_requests(1).await;

        client.finish().await;
        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("required H2 client auth wire test timed out");
}
