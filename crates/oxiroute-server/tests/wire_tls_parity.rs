#![cfg(unix)]
#![allow(dead_code, unused_imports, clippy::duplicate_mod)]
//! Policy that must mean the same on the OpenSSL and the rustls backend: the cipher list and the tickets sent.

mod support;

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use oxiroute_config::{AlpnProtocol, HttpVersionPolicy, TlsBackend, TlsPolicy, TlsVersion};
use rustls::{
    NamedGroup, SupportedCipherSuite,
    client::{ClientSessionStore, Resumption, Tls12ClientSessionValue, Tls13ClientSessionValue},
    pki_types::ServerName,
};
use tokio::time::timeout;

use support::{
    PROXY_SERVER_NAME, PlainH1Origin, ProxyHarness, ReservedListener, TEST_TIMEOUT, fixture,
    h1_request, proxy_config, tls_client_config, tls_client_config_with_versions,
    tls_connect_with_config,
};

const LONG_TIMEOUT: Duration = Duration::from_mins(1);
const BACKENDS: [TlsBackend; 2] = [TlsBackend::Openssl, TlsBackend::Rustls];

async fn with_listener<T>(
    backend: TlsBackend,
    policy: impl FnOnce(&mut TlsPolicy),
    test: impl AsyncFnOnce(&ProxyHarness) -> T,
) -> T {
    let origin = PlainH1Origin::start(b"parity").await;
    let reserved = ReservedListener::new();
    let mut config = proxy_config(
        reserved.address,
        origin.address,
        vec![AlpnProtocol::Http11],
        None,
        HttpVersionPolicy::default(),
    );
    config.tls_profiles[0].policy.backend = backend;
    policy(&mut config.tls_profiles[0].policy);
    let proxy = ProxyHarness::start(&config.validate(), reserved);
    let result = test(&proxy).await;
    proxy.finish().await;
    origin.finish().await;
    result
}

/// The TLS 1.2 suite a server with `cipher_list` picks for a client that offers every suite it has.
async fn tls12_suite(backend: TlsBackend, cipher_list: &str) -> SupportedCipherSuite {
    with_listener(
        backend,
        |policy| policy.cipher_list = Some(cipher_list.into()),
        async |proxy| {
            let client = tls_client_config_with_versions(
                &fixture("ca-a.pem"),
                &[b"http/1.1"],
                &[&rustls::version::TLS12],
            )
            .unwrap();
            let stream = tls_connect_with_config(proxy.address, PROXY_SERVER_NAME, client)
                .await
                .expect("TLS 1.2 connection to OxiRoute");
            stream.get_ref().1.negotiated_cipher_suite().unwrap()
        },
    )
    .await
}

#[tokio::test]
async fn a_cipher_list_picks_the_same_tls12_suite_on_both_backends() {
    timeout(LONG_TIMEOUT, async {
        for cipher_list in [
            "ECDHE-ECDSA-CHACHA20-POLY1305:ECDHE-ECDSA-AES256-GCM-SHA384",
            "ECDHE-ECDSA-AES256-GCM-SHA384:ECDHE-ECDSA-AES128-GCM-SHA256",
            "DHE-RSA-AES128-GCM-SHA256:ECDHE-ECDSA-AES128-GCM-SHA256:ECDHE-RSA-AES256-GCM-SHA384",
            "HIGH:!aNULL:!MD5",
            "ECDHE+AESGCM:!kRSA",
            "ECDHE-ECDSA-CHACHA20-POLY1305:HIGH:@STRENGTH",
        ] {
            let openssl = tls12_suite(TlsBackend::Openssl, cipher_list).await;
            let rustls = tls12_suite(TlsBackend::Rustls, cipher_list).await;
            assert_eq!(
                openssl.suite(),
                rustls.suite(),
                "cipher_list `{cipher_list}`"
            );
        }
    })
    .await
    .expect("cipher list wire test timed out");
}

#[tokio::test]
async fn a_cipher_list_never_offers_a_suite_it_leaves_out() {
    timeout(LONG_TIMEOUT, async {
        for backend in BACKENDS {
            let suite = tls12_suite(backend, "ECDHE-ECDSA-AES128-GCM-SHA256").await;
            assert_eq!(
                suite.suite(),
                rustls::CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256
            );
        }
    })
    .await
    .expect("cipher list wire test timed out");
}

/// Counts the TLS 1.3 tickets a client receives.
#[derive(Debug, Default)]
struct TicketCounter(AtomicUsize);

impl ClientSessionStore for TicketCounter {
    fn set_kx_hint(&self, _: ServerName<'static>, _: NamedGroup) {}
    fn kx_hint(&self, _: &ServerName<'_>) -> Option<NamedGroup> {
        None
    }
    fn set_tls12_session(&self, _: ServerName<'static>, _: Tls12ClientSessionValue) {}
    fn tls12_session(&self, _: &ServerName<'_>) -> Option<Tls12ClientSessionValue> {
        None
    }
    fn remove_tls12_session(&self, _: &ServerName<'static>) {}
    fn insert_tls13_ticket(&self, _: ServerName<'static>, _: Tls13ClientSessionValue) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
    fn take_tls13_ticket(&self, _: &ServerName<'static>) -> Option<Tls13ClientSessionValue> {
        None
    }
}

#[tokio::test]
async fn a_full_handshake_sends_one_ticket_and_a_session_cache_alone_sends_one_too() {
    timeout(LONG_TIMEOUT, async {
        for backend in BACKENDS {
            for (tickets, cache) in [(true, false), (false, true)] {
                let received = with_listener(
                    backend,
                    |policy| {
                        policy.session_tickets = tickets;
                        policy.session_cache = cache.then(|| oxiroute_config::TlsSessionCache {
                            name: "parity".into(),
                            size_bytes: 1 << 20,
                        });
                    },
                    async |proxy| {
                        let counter = Arc::new(TicketCounter::default());
                        let mut client = (*tls_client_config(&fixture("ca-a.pem"), &[b"http/1.1"])
                            .unwrap())
                        .clone();
                        client.resumption = Resumption::store(counter.clone());
                        let mut stream = tls_connect_with_config(
                            proxy.address,
                            PROXY_SERVER_NAME,
                            Arc::new(client),
                        )
                        .await
                        .expect("TLS connection to OxiRoute");
                        // Tickets follow the handshake; the client processes them while it reads the response.
                        let response = h1_request(&mut stream, "/tickets", true).await.unwrap();
                        assert_eq!(response.status, 200);
                        counter.0.load(Ordering::Relaxed)
                    },
                )
                .await;
                assert_eq!(
                    received, 1,
                    "{backend:?} with tickets {tickets} and cache {cache}"
                );
            }
        }
    })
    .await
    .expect("ticket count wire test timed out");
}
