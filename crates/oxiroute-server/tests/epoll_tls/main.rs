#![cfg(target_os = "linux")]
#![allow(dead_code, unused_imports, clippy::duplicate_mod)]
//! TLS terminated by the epoll data plane in the real binary, in front of a raw TCP origin: what it serves itself
//! (HTTP/1.1 over TLS 1.2 and 1.3), what it hands to the tokio path together with the established TLS session (an
//! HTTP/2 connection, a chunked upload), and when it is not used at all (not chosen, or a profile that is not rustls).
//! The debug line of the plane says which requests it served.

#[path = "../support/fixtures.rs"]
mod fixture_support;
#[path = "../support/process.rs"]
mod process_support;
#[path = "../support/mod.rs"]
mod support;

use oxiroute_config::{AlpnProtocol, ConfigDraft, HttpDataPlane, HttpVersionPolicy, TlsBackend};
use support::{PlainH1Origin, curl, proxy_config, run};

use process_support::{ServerProcess, reserve_tcp_address};

const SERVED_ON_EPOLL: &str = "served by the epoll data plane";

async fn through_proxy(
    edit: impl FnOnce(&mut ConfigDraft),
    exercise: impl AsyncFnOnce(u16, &PlainH1Origin),
) -> usize {
    let origin = PlainH1Origin::start(b"epoll-tls").await;
    let address = reserve_tcp_address();
    let mut proxy = proxy_config(
        address,
        origin.address,
        vec![AlpnProtocol::H2, AlpnProtocol::Http11],
        None,
        HttpVersionPolicy::default(),
    );
    proxy.log_filter = Some("oxiroute::epoll=debug".into());
    edit(&mut proxy);
    let config = proxy.validate();
    let mut server = ServerProcess::start_with_env(&config, None, &[]);
    server.wait_for_tcp(address).await;
    exercise(address.port(), &origin).await;
    let output = server.shutdown_output();
    origin.finish().await;
    output
        .lines()
        .filter(|line| line.contains(SERVED_ON_EPOLL))
        .count()
}

fn on_epoll(config: &mut ConfigDraft) {
    config.http_data_plane = Some(HttpDataPlane::Epoll);
    config.tls_profiles[0].policy.backend = TlsBackend::Rustls;
}

async fn fetch(port: u16, flags: &[&str]) {
    let (success, text) = run(curl(port, flags), b"").await;
    assert!(success, "curl {flags:?}: {text}");
    assert!(text.starts_with("epoll-tls"), "curl {flags:?}: {text}");
}

#[tokio::test]
async fn http_1_1_over_tls_12_and_13_is_served_by_the_epoll_plane() {
    let served = through_proxy(on_epoll, async |port, origin| {
        fetch(port, &["--http1.1", "--tlsv1.3"]).await;
        fetch(port, &["--http1.1", "--tlsv1.2", "--tls-max", "1.2"]).await;
        assert_eq!(origin.requests(), 2);
    })
    .await;
    assert_eq!(served, 2);
}

#[tokio::test]
async fn an_http2_client_continues_on_the_tokio_path_over_the_same_session() {
    let served = through_proxy(on_epoll, async |port, origin| {
        let (success, text) = run(curl(port, &["--http2"]), b"").await;
        assert!(success && text.contains("[200 http2]"), "{text}");
        assert_eq!(origin.requests(), 1);
    })
    .await;
    assert_eq!(served, 0);
}

#[tokio::test]
async fn a_chunked_upload_continues_on_the_tokio_path_with_the_plaintext_already_read() {
    let served = through_proxy(on_epoll, async |port, origin| {
        fetch(
            port,
            &[
                "--http1.1",
                "-H",
                "Transfer-Encoding: chunked",
                "--data-binary",
                "uploaded",
            ],
        )
        .await;
        assert!(origin.requests() >= 1);
    })
    .await;
    assert_eq!(served, 0);
}

#[tokio::test]
async fn a_tls_listener_stays_on_the_tokio_path_unless_epoll_is_chosen() {
    let served = through_proxy(
        |config| config.tls_profiles[0].policy.backend = TlsBackend::Rustls,
        async |port, _| fetch(port, &["--http1.1"]).await,
    )
    .await;
    assert_eq!(served, 0);
}

#[tokio::test]
async fn an_openssl_profile_stays_on_the_tokio_path_even_when_epoll_is_chosen() {
    let served = through_proxy(
        |config| {
            on_epoll(config);
            config.tls_profiles[0].policy.backend = TlsBackend::Openssl;
        },
        async |port, _| fetch(port, &["--http1.1"]).await,
    )
    .await;
    assert_eq!(served, 0);
}
