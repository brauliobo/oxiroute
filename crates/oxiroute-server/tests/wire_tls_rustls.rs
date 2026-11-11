#![cfg(unix)]
#![allow(dead_code, unused_imports, clippy::duplicate_mod)]
//! What the rustls backend and the OpenSSL backend must do alike, run against both in one process.

mod support;

use std::{sync::Arc, time::Duration};

use oxiroute_config::{
    AlpnProtocol, HttpVersionPolicy, TlsBackend, TlsPolicy, TlsSessionCache, TlsVersion,
};
use rustls::{HandshakeKind, ProtocolVersion, client::Resumption};
use tokio::time::{sleep, timeout};

use support::{
    PROXY_SERVER_NAME, PlainH1Origin, ProxyHarness, ReservedListener, TEST_TIMEOUT, fixture,
    h1_request, handshake_kind, negotiated_alpn, proxy_config, tls_client_config_with_versions,
    tls_connect_with_config,
};

const BACKENDS: [TlsBackend; 2] = [TlsBackend::Openssl, TlsBackend::Rustls];

struct Listener {
    proxy: ProxyHarness,
    origin: PlainH1Origin,
}

impl Listener {
    async fn start(
        backend: TlsBackend,
        alpn: Vec<AlpnProtocol>,
        min_version: TlsVersion,
        policy: impl FnOnce(&mut TlsPolicy),
    ) -> Self {
        let origin = PlainH1Origin::start(b"backend").await;
        let reserved = ReservedListener::new();
        let mut config = proxy_config(
            reserved.address,
            origin.address,
            alpn,
            None,
            HttpVersionPolicy::default(),
        );
        config.tls_profiles[0].min_version = min_version;
        config.tls_profiles[0].policy.backend = backend;
        policy(&mut config.tls_profiles[0].policy);
        let proxy = ProxyHarness::start(&config.validate(), reserved);
        Self { proxy, origin }
    }

    async fn finish(self) {
        self.proxy.finish().await;
        self.origin.finish().await;
    }
}

fn client(
    versions: &[&'static rustls::SupportedProtocolVersion],
    alpn: &[&[u8]],
) -> rustls::ClientConfig {
    (*tls_client_config_with_versions(&fixture("ca-a.pem"), alpn, versions).unwrap()).clone()
}

async fn request(client: &Arc<rustls::ClientConfig>, listener: &Listener) -> HandshakeKind {
    let mut stream = tls_connect_with_config(
        listener.proxy.address,
        PROXY_SERVER_NAME,
        Arc::clone(client),
    )
    .await
    .expect("TLS connection to OxiRoute");
    let response = h1_request(&mut stream, "/resume", true).await.unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(response.body, b"backend");
    handshake_kind(&stream).expect("completed handshake")
}

#[tokio::test]
async fn both_backends_negotiate_the_same_version_and_suite() {
    timeout(TEST_TIMEOUT, async {
        for versions in [
            &[&rustls::version::TLS13, &rustls::version::TLS12][..],
            &[&rustls::version::TLS12][..],
        ] {
            let mut negotiated = Vec::new();
            for backend in BACKENDS {
                let listener = Listener::start(
                    backend,
                    vec![AlpnProtocol::Http11],
                    TlsVersion::Tls12,
                    |_| {},
                )
                .await;
                let stream = tls_connect_with_config(
                    listener.proxy.address,
                    PROXY_SERVER_NAME,
                    Arc::new(client(versions, &[b"http/1.1"])),
                )
                .await
                .expect("TLS connection to OxiRoute");
                let connection = stream.get_ref().1;
                negotiated.push((
                    connection.protocol_version(),
                    connection
                        .negotiated_cipher_suite()
                        .map(|suite| suite.suite()),
                ));
                drop(stream);
                listener.finish().await;
            }
            assert_eq!(negotiated[0], negotiated[1], "{negotiated:?}");
        }
    })
    .await
    .expect("suite parity wire test timed out");
}

#[tokio::test]
async fn a_tls_1_3_minimum_refuses_a_tls_1_2_client_on_both_backends() {
    timeout(TEST_TIMEOUT, async {
        for backend in BACKENDS {
            let listener = Listener::start(
                backend,
                vec![AlpnProtocol::Http11],
                TlsVersion::Tls13,
                |_| {},
            )
            .await;
            let tls12 = Arc::new(client(&[&rustls::version::TLS12], &[b"http/1.1"]));
            assert!(
                tls_connect_with_config(listener.proxy.address, PROXY_SERVER_NAME, tls12)
                    .await
                    .is_err(),
                "{backend:?} accepted TLS 1.2 under a TLS 1.3 minimum"
            );
            let tls13 = Arc::new(client(&[&rustls::version::TLS13], &[b"http/1.1"]));
            let stream = tls_connect_with_config(listener.proxy.address, PROXY_SERVER_NAME, tls13)
                .await
                .expect("TLS 1.3 connection");
            assert_eq!(
                stream.get_ref().1.protocol_version(),
                Some(ProtocolVersion::TLSv1_3)
            );
            drop(stream);
            listener.finish().await;
        }
    })
    .await
    .expect("minimum version wire test timed out");
}

#[tokio::test]
async fn a_client_offering_only_unknown_protocols_is_served_without_alpn_unless_the_profile_is_h2_only()
 {
    timeout(TEST_TIMEOUT, async {
        for backend in BACKENDS {
            let flexible = Listener::start(
                backend,
                vec![AlpnProtocol::H2, AlpnProtocol::Http11],
                TlsVersion::Tls12,
                |_| {},
            )
            .await;
            let config = Arc::new(client(&[&rustls::version::TLS13], &[b"unknown/1"]));
            let mut stream =
                tls_connect_with_config(flexible.proxy.address, PROXY_SERVER_NAME, config)
                    .await
                    .expect("a client with no common protocol is still served");
            assert_eq!(negotiated_alpn(&stream), None, "{backend:?}");
            assert_eq!(
                h1_request(&mut stream, "/plain", true)
                    .await
                    .unwrap()
                    .status,
                200
            );
            drop(stream);
            flexible.finish().await;

            let h2_only =
                Listener::start(backend, vec![AlpnProtocol::H2], TlsVersion::Tls12, |_| {}).await;
            let config = Arc::new(client(&[&rustls::version::TLS13], &[b"unknown/1"]));
            assert!(
                tls_connect_with_config(h2_only.proxy.address, PROXY_SERVER_NAME, config)
                    .await
                    .is_err(),
                "{backend:?} served a client without h2 on an h2-only profile"
            );
            h2_only.finish().await;
        }
    })
    .await
    .expect("ALPN fallback wire test timed out");
}

#[tokio::test]
async fn sessions_resume_through_the_cache_and_through_tickets_on_both_backends() {
    timeout(TEST_TIMEOUT, async {
        for backend in BACKENDS {
            for (versions, label) in [
                (&[&rustls::version::TLS12][..], "TLS 1.2"),
                (&[&rustls::version::TLS13][..], "TLS 1.3"),
            ] {
                for (cache, tickets) in [(true, false), (false, true)] {
                    let listener = Listener::start(
                        backend,
                        vec![AlpnProtocol::Http11],
                        TlsVersion::Tls12,
                        |policy| {
                            policy.session_tickets = tickets;
                            policy.session_cache = cache.then(|| TlsSessionCache {
                                name: "wire".into(),
                                size_bytes: 1024 * 1024,
                            });
                        },
                    )
                    .await;
                    let mut config = client(versions, &[b"http/1.1"]);
                    config.resumption = Resumption::in_memory_sessions(8);
                    let config = Arc::new(config);

                    assert_eq!(request(&config, &listener).await, HandshakeKind::Full);
                    assert_eq!(
                        request(&config, &listener).await,
                        HandshakeKind::Resumed,
                        "{backend:?} {label} cache={cache} tickets={tickets}"
                    );
                    listener.finish().await;
                }
            }
        }
    })
    .await
    .expect("resumption wire test timed out");
}

#[tokio::test]
async fn nothing_resumes_without_a_cache_or_tickets() {
    timeout(TEST_TIMEOUT, async {
        for backend in BACKENDS {
            let listener = Listener::start(
                backend,
                vec![AlpnProtocol::Http11],
                TlsVersion::Tls12,
                |_| {},
            )
            .await;
            let mut config = client(
                &[&rustls::version::TLS12, &rustls::version::TLS13],
                &[b"http/1.1"],
            );
            config.resumption = Resumption::in_memory_sessions(8);
            let config = Arc::new(config);

            assert_eq!(request(&config, &listener).await, HandshakeKind::Full);
            assert_eq!(
                request(&config, &listener).await,
                HandshakeKind::Full,
                "{backend:?}"
            );
            listener.finish().await;
        }
    })
    .await
    .expect("no-resumption wire test timed out");
}

#[tokio::test]
async fn a_session_older_than_the_timeout_does_not_resume() {
    timeout(Duration::from_secs(20), async {
        for backend in BACKENDS {
            let listener = Listener::start(
                backend,
                vec![AlpnProtocol::Http11],
                TlsVersion::Tls12,
                |policy| {
                    policy.session_timeout_seconds = Some(1);
                    policy.session_cache = Some(TlsSessionCache {
                        name: "wire".into(),
                        size_bytes: 1024 * 1024,
                    });
                },
            )
            .await;
            let mut config = client(&[&rustls::version::TLS12], &[b"http/1.1"]);
            config.resumption = Resumption::in_memory_sessions(8);
            let config = Arc::new(config);

            assert_eq!(request(&config, &listener).await, HandshakeKind::Full);
            assert_eq!(
                request(&config, &listener).await,
                HandshakeKind::Resumed,
                "{backend:?}"
            );
            sleep(Duration::from_millis(2_200)).await;
            assert_eq!(
                request(&config, &listener).await,
                HandshakeKind::Full,
                "{backend:?} after the timeout"
            );
            listener.finish().await;
        }
    })
    .await
    .expect("session timeout wire test timed out");
}
