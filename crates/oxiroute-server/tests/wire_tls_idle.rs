#![cfg(unix)]
#![allow(dead_code, unused_imports, clippy::duplicate_mod)]
//! The wait between two requests of a keep-alive HTTP/1 connection over TLS, as seen on the wire: how long it may last,
//! what ends it, and that the request that follows is served as if the connection had never been idle. The proxy is the
//! real binary; every scenario runs with the fast lane (which serves the connection itself) and without it (a session
//! serves every request).

#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/process.rs"]
mod process_support;
mod support;

use std::{
    net::SocketAddr,
    time::{Duration, Instant},
};

use oxiroute_config::{AlpnProtocol, DownstreamTimeoutPolicy, HttpVersionPolicy};
use process_support::ServerProcess;
use rustls::version::{TLS12, TLS13};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    time::{sleep, timeout},
};

use support::{
    PROXY_SERVER_NAME, PlainH1Origin, ReservedListener, TEST_TIMEOUT, ValidatedProxyConfig,
    fixture, h1_request, proxy_config, tls_client_config_with_versions, tls_connect_with_config,
};

type Client = tokio_rustls::client::TlsStream<tokio::net::TcpStream>;

#[derive(Clone, Copy)]
enum Path {
    /// A session serves every request.
    Classic,
    /// The fast lane serves the requests it can.
    Lane,
}

struct Setup {
    address: SocketAddr,
    server: ServerProcess,
    origin: PlainH1Origin,
    _config: ValidatedProxyConfig,
}

async fn setup(path: Path, keepalive_ms: Option<u64>, request_ms: Option<u64>) -> Setup {
    let origin = PlainH1Origin::start(b"idle").await;
    let address = ReservedListener::new().address;
    let mut config = proxy_config(
        address,
        origin.address,
        vec![AlpnProtocol::Http11],
        None,
        HttpVersionPolicy::default(),
    );
    config.http_fast_lane = Some(matches!(path, Path::Lane));
    config.listeners[0].downstream_timeouts = DownstreamTimeoutPolicy {
        client_timeout_ms: request_ms,
        request_timeout_ms: request_ms,
        keepalive_timeout_ms: keepalive_ms,
    };
    let config = config.validate();
    let mut server = ServerProcess::start(&config, None);
    server.wait_for_tcp(address).await;
    Setup {
        address,
        server,
        origin,
        _config: config,
    }
}

impl Setup {
    async fn connect(&self, versions: &[&'static rustls::SupportedProtocolVersion]) -> Client {
        let config =
            tls_client_config_with_versions(&fixture("ca-a.pem"), &[b"http/1.1"], versions)
                .unwrap();
        tls_connect_with_config(self.address, PROXY_SERVER_NAME, config)
            .await
            .expect("TLS connection to OxiRoute")
    }

    async fn finish(self) {
        self.server.shutdown();
        self.origin.finish().await;
    }
}

async fn serve(stream: &mut Client) {
    let response = h1_request(stream, "/idle", false).await.expect("response");
    assert_eq!(response.status, 200);
    assert_eq!(response.body, b"idle");
}

/// Waits until the proxy ends the connection; returns how long it took.
async fn closed_after(stream: &mut Client) -> Duration {
    let started = Instant::now();
    let mut buffer = [0_u8; 16];
    let read = timeout(TEST_TIMEOUT, stream.read(&mut buffer))
        .await
        .expect("the proxy never closed the idle connection");
    assert!(matches!(read, Ok(0) | Err(_)), "unexpected bytes: {read:?}");
    started.elapsed()
}

async fn stays_open_for(stream: &mut Client, wait: Duration) -> bool {
    let mut buffer = [0_u8; 16];
    timeout(wait, stream.read(&mut buffer)).await.is_err()
}

/// One test per path for a scenario.
macro_rules! on_both_paths {
    ($name:ident) => {
        mod $name {
            use super::*;

            #[tokio::test]
            async fn classic_path() {
                super::$name(Path::Classic).await;
            }

            #[tokio::test]
            async fn lane() {
                super::$name(Path::Lane).await;
            }
        }
    };
}

async fn an_idle_tls_connection_is_closed_when_the_keep_alive_timeout_ends(path: Path) {
    let setup = setup(path, Some(300), None).await;
    let mut stream = setup.connect(&[&TLS13]).await;
    serve(&mut stream).await;
    let idle = closed_after(&mut stream).await;
    assert!(idle >= Duration::from_millis(200), "closed after {idle:?}");
    setup.finish().await;
}
on_both_paths!(an_idle_tls_connection_is_closed_when_the_keep_alive_timeout_ends);

async fn a_tls_connection_idle_for_less_than_the_timeout_serves_the_next_request(path: Path) {
    for versions in [&[&TLS13][..], &[&TLS12][..]] {
        let setup = setup(path, Some(1_500), None).await;
        let mut stream = setup.connect(versions).await;
        for _ in 0..3 {
            serve(&mut stream).await;
            assert!(stays_open_for(&mut stream, Duration::from_millis(300)).await);
        }
        assert_eq!(setup.origin.requests(), 3);
        setup.finish().await;
    }
}
on_both_paths!(a_tls_connection_idle_for_less_than_the_timeout_serves_the_next_request);

async fn the_idle_wait_over_tls_does_not_count_against_the_request_timeout(path: Path) {
    let setup = setup(path, Some(5_000), Some(400)).await;
    let mut stream = setup.connect(&[&TLS13]).await;
    serve(&mut stream).await;
    assert!(stays_open_for(&mut stream, Duration::from_millis(900)).await);
    serve(&mut stream).await;
    setup.finish().await;
}
on_both_paths!(the_idle_wait_over_tls_does_not_count_against_the_request_timeout);

async fn the_request_timeout_runs_from_the_first_byte_after_a_tls_idle_wait(path: Path) {
    let setup = setup(path, Some(5_000), Some(500)).await;
    let mut stream = setup.connect(&[&TLS13]).await;
    serve(&mut stream).await;
    sleep(Duration::from_millis(200)).await;
    stream.write_all(b"GET / HT").await.unwrap();
    stream.flush().await.unwrap();
    assert!(stays_open_for(&mut stream, Duration::from_millis(200)).await);
    let stalled = closed_after(&mut stream).await;
    assert!(stalled < Duration::from_secs(3), "closed after {stalled:?}");
    setup.finish().await;
}
on_both_paths!(the_request_timeout_runs_from_the_first_byte_after_a_tls_idle_wait);

async fn a_head_split_across_the_tls_idle_wake_up_is_parsed(path: Path) {
    let setup = setup(path, None, None).await;
    let mut stream = setup.connect(&[&TLS13]).await;
    serve(&mut stream).await;
    sleep(Duration::from_millis(100)).await;
    stream.write_all(b"GET /idle HTTP/1.1\r\n").await.unwrap();
    stream.flush().await.unwrap();
    sleep(Duration::from_millis(100)).await;
    let rest = format!("Host: {PROXY_SERVER_NAME}\r\n\r\n");
    stream.write_all(rest.as_bytes()).await.unwrap();
    stream.flush().await.unwrap();
    let mut response = vec![0_u8; 1024];
    let count = timeout(TEST_TIMEOUT, stream.read(&mut response))
        .await
        .expect("response")
        .unwrap();
    assert!(response[..count].starts_with(b"HTTP/1.1 200"));
    setup.finish().await;
}
on_both_paths!(a_head_split_across_the_tls_idle_wake_up_is_parsed);

async fn back_to_back_requests_on_one_tls_connection_are_each_answered(path: Path) {
    let setup = setup(path, None, None).await;
    let mut stream = setup.connect(&[&TLS13]).await;
    for _ in 0..50 {
        serve(&mut stream).await;
    }
    assert_eq!(setup.origin.requests(), 50);
    setup.finish().await;
}
on_both_paths!(back_to_back_requests_on_one_tls_connection_are_each_answered);

async fn a_close_notify_while_idle_ends_only_that_connection(path: Path) {
    let setup = setup(path, None, None).await;
    let mut leaving = setup.connect(&[&TLS13]).await;
    let mut staying = setup.connect(&[&TLS13]).await;
    serve(&mut leaving).await;
    serve(&mut staying).await;
    sleep(Duration::from_millis(300)).await;
    leaving.shutdown().await.unwrap();
    sleep(Duration::from_millis(300)).await;
    serve(&mut staying).await;
    setup.finish().await;
}
on_both_paths!(a_close_notify_while_idle_ends_only_that_connection);

async fn many_idle_tls_connections_are_each_served_when_they_wake(path: Path) {
    let setup = setup(path, None, None).await;
    let mut streams = Vec::new();
    for _ in 0..90 {
        let mut stream = setup.connect(&[&TLS13]).await;
        serve(&mut stream).await;
        streams.push(stream);
    }
    sleep(Duration::from_millis(300)).await;
    sleep(Duration::from_millis(200)).await;
    for stream in streams.iter_mut().rev() {
        serve(stream).await;
    }
    assert_eq!(setup.origin.requests(), 180);
    drop(streams);
    setup.finish().await;
}
on_both_paths!(many_idle_tls_connections_are_each_served_when_they_wake);

async fn a_stopping_server_closes_idle_tls_connections(path: Path) {
    let setup = setup(path, None, None).await;
    let mut stream = setup.connect(&[&TLS13]).await;
    serve(&mut stream).await;
    sleep(Duration::from_millis(100)).await;
    let Setup {
        server,
        origin,
        _config,
        ..
    } = setup;
    let stopping = tokio::task::spawn_blocking(move || server.shutdown_gracefully());
    let (stopped, closed) = tokio::join!(stopping, closed_after(&mut stream));
    stopped.unwrap();
    assert!(closed < TEST_TIMEOUT);
    origin.finish().await;
}
on_both_paths!(a_stopping_server_closes_idle_tls_connections);
