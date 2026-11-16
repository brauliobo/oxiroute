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
#[path = "support/process.rs"]
mod process_support;

use std::{future::Future, net::SocketAddr, sync::Arc, time::Duration};

use oxiroute_config::{ConfigDraft, HttpDataPlane};
use process_support::{ServerProcess, reserve_tcp_address};
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
    server: ServerProcess,
}

impl Proxy {
    async fn start(
        origin: SocketAddr,
        plane: HttpDataPlane,
        edit: impl FnOnce(&mut ConfigDraft),
    ) -> Self {
        let address = reserve_tcp_address();
        let mut config = h1_wire::config(address, origin);
        config.log_filter = Some("oxiroute::epoll=debug".into());
        config.http_data_plane = Some(plane);
        config.http_services[0].upstream_io_timeout_ms = 30_000;
        edit(&mut config);
        let config = config.validate().expect("valid epoll safety config");
        let mut server = ServerProcess::start_with_env(&config, None, &[]);
        server.wait_for_tcp(address).await;
        Self { address, server }
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
    let mut rest = Vec::new();
    let _ = timeout(STEP, stream.read_to_end(&mut rest))
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
