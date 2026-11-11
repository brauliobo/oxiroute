#![cfg(unix)]
#![allow(dead_code, unused_imports, clippy::duplicate_mod)]
//! The profile's `key_exchange_groups` decide which group a real client negotiates; unset keeps the default.

mod support;

use std::sync::Arc;

use oxiroute_config::{
    AlpnProtocol, HttpVersionPolicy, TlsBackend, TlsKeyExchangeGroup, TlsVersion,
};
use rustls::NamedGroup;
use tokio::time::timeout;

use support::{
    PROXY_SERVER_NAME, PlainH1Origin, ProxyHarness, ReservedListener, TEST_TIMEOUT, fixture,
    h1_request, proxy_config, tls_client_config_with_groups, tls_connect_with_config,
};

const BACKENDS: [TlsBackend; 2] = [TlsBackend::Openssl, TlsBackend::Rustls];

/// The group a client that offers `offered` negotiates with a profile listing `groups`, or `None` when the handshake fails.
async fn negotiated(
    backend: TlsBackend,
    groups: Option<Vec<TlsKeyExchangeGroup>>,
    offered: &[NamedGroup],
) -> Option<NamedGroup> {
    let origin = PlainH1Origin::start(b"groups").await;
    let reserved = ReservedListener::new();
    let mut config = proxy_config(
        reserved.address,
        origin.address,
        vec![AlpnProtocol::Http11],
        None,
        HttpVersionPolicy::default(),
    );
    config.tls_profiles[0].policy.backend = backend;
    config.tls_profiles[0].policy.key_exchange_groups = groups;
    let proxy = ProxyHarness::start(&config.validate(), reserved);

    let client =
        tls_client_config_with_groups(&fixture("ca-a.pem"), &[b"http/1.1"], offered).unwrap();
    let group = match tls_connect_with_config(proxy.address, PROXY_SERVER_NAME, client).await {
        Ok(mut stream) => {
            let group = stream
                .get_ref()
                .1
                .negotiated_key_exchange_group()
                .map(rustls::crypto::SupportedKxGroup::name);
            let response = h1_request(&mut stream, "/groups", true).await.unwrap();
            assert_eq!(response.status, 200);
            group
        }
        Err(_) => None,
    };
    proxy.finish().await;
    origin.finish().await;
    group
}

const CLASSICAL: [NamedGroup; 3] = [
    NamedGroup::X25519,
    NamedGroup::secp256r1,
    NamedGroup::secp384r1,
];

#[tokio::test]
async fn the_default_list_is_unchanged_and_picks_the_clients_first_share() {
    timeout(TEST_TIMEOUT, async {
        for backend in BACKENDS {
            assert_eq!(
                negotiated(backend, None, &CLASSICAL).await,
                Some(NamedGroup::X25519)
            );
            assert_eq!(
                negotiated(backend, None, &[NamedGroup::secp256r1, NamedGroup::X25519]).await,
                Some(NamedGroup::secp256r1)
            );
        }
    })
    .await
    .expect("key exchange wire test timed out");
}

#[tokio::test]
async fn a_listed_group_is_negotiated() {
    timeout(TEST_TIMEOUT, async {
        for backend in BACKENDS {
            use TlsKeyExchangeGroup::{Secp256r1, Secp384r1, X25519};
            // The client's key share (X25519) is listed: no retry round trip, whatever the order.
            assert_eq!(
                negotiated(backend, Some(vec![Secp256r1, X25519]), &CLASSICAL).await,
                Some(NamedGroup::X25519)
            );
            // No listed group has a share: the server asks for a retry with a listed group.
            assert_eq!(
                negotiated(backend, Some(vec![Secp256r1]), &CLASSICAL).await,
                Some(NamedGroup::secp256r1)
            );
            assert_eq!(
                negotiated(backend, Some(vec![Secp384r1]), &CLASSICAL).await,
                Some(NamedGroup::secp384r1)
            );
        }
    })
    .await
    .expect("key exchange wire test timed out");
}

#[tokio::test]
async fn the_servers_order_chooses_the_retry_group() {
    timeout(TEST_TIMEOUT, async {
        for backend in BACKENDS {
            use TlsKeyExchangeGroup::{Secp256r1, Secp384r1};
            assert_eq!(
                negotiated(backend, Some(vec![Secp384r1, Secp256r1]), &CLASSICAL).await,
                Some(NamedGroup::secp384r1)
            );
            assert_eq!(
                negotiated(backend, Some(vec![Secp256r1, Secp384r1]), &CLASSICAL).await,
                Some(NamedGroup::secp256r1)
            );
        }
    })
    .await
    .expect("key exchange wire test timed out");
}

#[tokio::test]
async fn a_client_without_a_listed_group_cannot_connect() {
    timeout(TEST_TIMEOUT, async {
        for backend in BACKENDS {
            let groups = Some(vec![TlsKeyExchangeGroup::Secp256r1]);
            assert_eq!(
                negotiated(backend, groups, &[NamedGroup::X25519]).await,
                None
            );
        }
    })
    .await
    .expect("key exchange wire test timed out");
}

/// Whether a browser-like OpenSSL client (key shares for the hybrid group and X25519) ended on a plain X25519 exchange.
async fn browser_hello_negotiates_x25519(
    backend: TlsBackend,
    groups: Option<Vec<TlsKeyExchangeGroup>>,
) -> bool {
    use openssl::{
        pkey::Id,
        ssl::{SslConnector, SslMethod, SslVerifyMode},
    };
    let origin = PlainH1Origin::start(b"groups").await;
    let reserved = ReservedListener::new();
    let mut config = proxy_config(
        reserved.address,
        origin.address,
        vec![AlpnProtocol::Http11],
        None,
        HttpVersionPolicy::default(),
    );
    config.tls_profiles[0].policy.backend = backend;
    config.tls_profiles[0].policy.key_exchange_groups = groups;
    let proxy = ProxyHarness::start(&config.validate(), reserved);
    let address = proxy.address;
    let x25519 = tokio::task::spawn_blocking(move || {
        let mut connector = SslConnector::builder(SslMethod::tls_client()).unwrap();
        connector.set_verify(SslVerifyMode::NONE);
        connector
            .set_groups_list("*X25519MLKEM768:*X25519")
            .unwrap();
        let stream = std::net::TcpStream::connect(address).unwrap();
        let stream = connector
            .build()
            .connect(PROXY_SERVER_NAME, stream)
            .unwrap();
        let id = stream.ssl().peer_tmp_key().map(|key| key.id());
        eprintln!("peer_tmp_key: {id:?}");
        matches!(id, Ok(Id::X25519))
    })
    .await
    .unwrap();
    proxy.finish().await;
    origin.finish().await;
    x25519
}

#[tokio::test]
async fn a_browser_hello_gets_the_hybrid_group_by_default_and_x25519_when_the_list_leaves_it_out() {
    timeout(TEST_TIMEOUT, async {
        for backend in BACKENDS {
            assert!(!browser_hello_negotiates_x25519(backend, None).await);
            let classical = vec![TlsKeyExchangeGroup::X25519, TlsKeyExchangeGroup::Secp256r1];
            assert!(browser_hello_negotiates_x25519(backend, Some(classical)).await);
        }
    })
    .await
    .expect("key exchange wire test timed out");
}
