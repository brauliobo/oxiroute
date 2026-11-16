#![cfg(target_os = "linux")]
#![allow(dead_code, unused_imports, clippy::duplicate_mod)]
//! What the epoll data plane must hold on to in production: the real binary in front of raw loopback origins, each
//! scenario on the epoll plane and, where the tokio plane has the same contract, on the tokio plane too.

#[path = "support/config.rs"]
mod config_support;
#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/h1_wire.rs"]
mod h1_wire;
#[path = "support/http.rs"]
mod http_support;
#[path = "support/process.rs"]
mod process_support;

use std::{
    future::Future,
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};

use http_support::http_request;
use oxiroute_config::{ConfigDraft, HttpDataPlane, Management};
use process_support::{ServerProcess, reserve_tcp_address, write_config};
use rustix::net::sockopt::set_socket_linger;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    sync::Notify,
    task::JoinHandle,
    time::timeout,
};

const GET: &[u8] = b"GET /r HTTP/1.1\r\nHost: example.test\r\n\r\n";
const SERVED_ON_EPOLL: &str = "served by the epoll data plane";
const STEP: Duration = Duration::from_secs(10);
const TOKEN: &str = "5b1f0c6e8a3d4f27b9c0e1d2a3f4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e0f1a2";

macro_rules! on_both_planes {
    ($scenario:ident) => {
        mod $scenario {
            #[tokio::test]
            async fn epoll() {
                super::$scenario(super::HttpDataPlane::Epoll).await;
            }

            #[tokio::test]
            async fn tokio() {
                super::$scenario(super::HttpDataPlane::Tokio).await;
            }
        }
    };
}

struct Scripted {
    address: SocketAddr,
    task: JoinHandle<()>,
}

impl Scripted {
    async fn start<F, Fut>(script: F) -> Self
    where
        F: Fn(TcpStream) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
        let address = listener.local_addr().expect("origin address");
        let script = Arc::new(script);
        let task = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.expect("origin accept");
                let script = Arc::clone(&script);
                tokio::spawn(async move { script(stream).await });
            }
        });
        Self { address, task }
    }
}

impl Drop for Scripted {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Proxy {
    address: SocketAddr,
    management: SocketAddr,
    config: ConfigDraft,
    server: ServerProcess,
}

impl Proxy {
    async fn start(
        origin: SocketAddr,
        plane: HttpDataPlane,
        edit: impl FnOnce(&mut ConfigDraft),
    ) -> Self {
        let (address, management) = (reserve_tcp_address(), reserve_tcp_address());
        let mut config = h1_wire::config(address, origin);
        config.management = Some(Management {
            bind: management,
            ui_dir: None,
            token_file: None,
        });
        config.log_filter = Some("oxiroute::epoll=debug".into());
        config.http_data_plane = Some(plane);
        config.http_services[0].upstream_io_timeout_ms = 30_000;
        edit(&mut config);
        let validated = config
            .clone()
            .validate()
            .expect("valid epoll safety config");
        let mut server = ServerProcess::start_with_env(&validated, Some(TOKEN), &[]);
        server.wait_for_tcp(address).await;
        server.wait_for_tcp(management).await;
        Self {
            address,
            management,
            config,
            server,
        }
    }

    async fn revision(&self) -> String {
        let authorization = format!("Bearer {TOKEN}");
        let response = http_request(
            self.management,
            "GET",
            "/api/v1/generations",
            &[("Authorization", &authorization)],
            &[],
        )
        .await;
        response.json()["generation"]["activeRevision"]
            .as_str()
            .expect("active revision")
            .to_owned()
    }

    async fn reload(&mut self) {
        let before = self.revision().await;
        self.config.max_connections = Some(self.config.max_connections.unwrap_or(1_000) + 1);
        let validated = self.config.clone().validate().expect("valid reload config");
        write_config(&self.server.config_path, &validated);
        timeout(STEP, async {
            while self.revision().await == before {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("the reloaded generation activated");
    }

    async fn connect(&self) -> TcpStream {
        TcpStream::connect(self.address).await.expect("connect")
    }

    fn served_on_epoll(self) -> usize {
        self.server
            .shutdown_output()
            .lines()
            .filter(|line| line.contains(SERVED_ON_EPOLL))
            .count()
    }
}

fn expected_on_epoll(plane: HttpDataPlane, requests: usize) -> usize {
    if plane == HttpDataPlane::Epoll {
        requests
    } else {
        0
    }
}

async fn read_head(stream: &mut TcpStream) -> Vec<u8> {
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        let mut byte = [0_u8; 1];
        match stream.read(&mut byte).await {
            Ok(1) => head.push(byte[0]),
            _ => break,
        }
    }
    head
}

async fn read_until(stream: &mut TcpStream, needle: &str) -> String {
    let mut received = Vec::new();
    while !String::from_utf8_lossy(&received).contains(needle) {
        let mut chunk = [0_u8; 4096];
        let count = timeout(STEP, stream.read(&mut chunk))
            .await
            .expect("the proxy answered in time")
            .expect("read");
        assert_ne!(count, 0, "closed before `{needle}`");
        received.extend_from_slice(&chunk[..count]);
    }
    String::from_utf8_lossy(&received).into_owned()
}

async fn read_to_end(stream: &mut TcpStream) -> String {
    read_to_end_within(stream, STEP).await
}

async fn read_to_end_within(stream: &mut TcpStream, limit: Duration) -> String {
    let mut rest = Vec::new();
    let _ = timeout(limit, stream.read_to_end(&mut rest))
        .await
        .expect("the proxy ended the connection in time");
    String::from_utf8_lossy(&rest).into_owned()
}

fn reset(stream: TcpStream) {
    let stream = stream.into_std().expect("std stream");
    set_socket_linger(&stream, Some(Duration::ZERO)).expect("zero linger");
    drop(stream);
}

async fn a_body_ended_by_an_origin_reset_is_not_completed(plane: HttpDataPlane) {
    let reset_now = Arc::new(Notify::new());
    let signal = Arc::clone(&reset_now);
    let origin = Scripted::start(move |mut stream| {
        let signal = Arc::clone(&signal);
        async move {
            read_head(&mut stream).await;
            stream
                .write_all(b"HTTP/1.0 200 OK\r\n\r\npart-one;")
                .await
                .expect("origin write");
            signal.notified().await;
            reset(stream);
        }
    })
    .await;
    let proxy = Proxy::start(origin.address, plane, |_| {}).await;
    let mut client = proxy.connect().await;
    client.write_all(GET).await.unwrap();
    let mut received = read_until(&mut client, "part-one;").await;
    reset_now.notify_one();
    received.push_str(&read_to_end(&mut client).await);
    assert!(
        !received.ends_with("0\r\n\r\n"),
        "a reset origin completed the body: {received:?}"
    );
    assert_eq!(proxy.served_on_epoll(), expected_on_epoll(plane, 1));
}

on_both_planes!(a_body_ended_by_an_origin_reset_is_not_completed);

async fn a_response_in_flight_survives_the_replacement_of_its_generation(plane: HttpDataPlane) {
    const SECONDS: usize = 36;
    let origin = Scripted::start(|mut stream| async move {
        read_head(&mut stream).await;
        let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {SECONDS}\r\n\r\n");
        stream
            .write_all(head.as_bytes())
            .await
            .expect("origin head");
        for _ in 0..SECONDS {
            if stream.write_all(b"y").await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    })
    .await;
    let mut proxy = Proxy::start(origin.address, plane, |config| {
        config.http_services[0].upstream_io_timeout_ms = 120_000;
    })
    .await;
    let mut client = proxy.connect().await;
    client.write_all(GET).await.unwrap();
    read_until(&mut client, "\r\n\r\ny").await;
    proxy.reload().await;
    let rest = read_to_end_within(&mut client, Duration::from_secs(60)).await;
    assert_eq!(rest.len(), SECONDS - 1, "the response was cut: {rest:?}");
    assert_eq!(proxy.served_on_epoll(), expected_on_epoll(plane, 1));
}

on_both_planes!(a_response_in_flight_survives_the_replacement_of_its_generation);

async fn a_partial_request_head_is_closed_at_the_default_read_limit(plane: HttpDataPlane) {
    let origin = Scripted::start(|_| async {}).await;
    let proxy = Proxy::start(origin.address, plane, |_| {}).await;
    let mut client = proxy.connect().await;
    client
        .write_all(b"GET /r HTTP/1.1\r\nHost: example.test\r\nX-A:")
        .await
        .unwrap();
    let started = Instant::now();
    let _ = timeout(Duration::from_secs(90), client.read_to_end(&mut Vec::new()))
        .await
        .expect("a partial head stayed open past 90 seconds");
    let open = started.elapsed();
    assert!(
        (Duration::from_secs(55)..Duration::from_secs(75)).contains(&open),
        "a partial head was closed after {open:?}"
    );
}

on_both_planes!(a_partial_request_head_is_closed_at_the_default_read_limit);
