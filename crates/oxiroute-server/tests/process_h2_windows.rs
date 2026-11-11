#![cfg(unix)]
#![allow(
    dead_code,
    unused_imports,
    clippy::duplicate_mod,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::doc_markdown
)]
//! The HTTP/2 receive windows of a listener, on the wire of the real binary: what its SETTINGS and connection
//! `WINDOW_UPDATE` advertise, and how many `WINDOW_UPDATE`s an upload through it costs.

#[path = "support/config.rs"]
mod config_support;
#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/http.rs"]
mod http_support;
#[path = "support/process.rs"]
mod process_support;
#[path = "support/rtmp.rs"]
mod rtmp_support;
#[path = "support/sse.rs"]
mod sse_support;
#[path = "support/mod.rs"]
mod wire_support;

use std::{net::SocketAddr, time::Duration};

use oxiroute_config::{AlpnProtocol, HttpVersionPolicy};
use process_support::{ServerProcess, reserve_tcp_address};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
    time::timeout,
};

const MIB: usize = 1 << 20;
const STEP: Duration = Duration::from_secs(20);
const PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
const FRAME: usize = 16_384;
const DATA: u8 = 0;
const HEADERS: u8 = 1;
const SETTINGS: u8 = 4;
const WINDOW_UPDATE: u8 = 8;

type Tls = tokio_rustls::client::TlsStream<TcpStream>;

/// An HTTP/1.1 origin that reads each request to its end (chunked) and answers 200 without a body.
async fn origin() -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
    let address = listener.local_addr().expect("origin address");
    let task = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.expect("origin accept");
            tokio::spawn(async move {
                let mut seen = Vec::new();
                let mut buffer = vec![0; 64 * 1024];
                loop {
                    let Ok(count @ 1..) = stream.read(&mut buffer).await else {
                        return;
                    };
                    seen.extend_from_slice(&buffer[..count]);
                    if seen.ends_with(b"0\r\n\r\n") {
                        let reply = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";
                        if stream.write_all(reply).await.is_err() {
                            return;
                        }
                        seen.clear();
                    }
                }
            });
        }
    });
    (address, task)
}

struct Proxy {
    process: ServerProcess,
    address: SocketAddr,
    origin: JoinHandle<()>,
    _config: wire_support::ValidatedProxyConfig,
}

async fn start(windows: Option<(u64, u64)>) -> Proxy {
    let (origin_address, origin) = origin().await;
    let address = reserve_tcp_address();
    let mut config = wire_support::proxy_config(
        address,
        origin_address,
        vec![AlpnProtocol::H2],
        None,
        HttpVersionPolicy::default(),
    );
    config.http_services[0].max_request_body_bytes = Some(64 * MIB as u64);
    config.http_services[0].upstream_io_timeout_ms = 60_000;
    if let Some((stream, connection)) = windows {
        config.listeners[0].h2_initial_stream_window = Some(stream);
        config.listeners[0].h2_initial_connection_window = Some(connection);
    }
    let config = config.validate();
    let mut process = ServerProcess::start(&config, None);
    process.wait_for_tcp(address).await;
    Proxy {
        process,
        address,
        origin,
        _config: config,
    }
}

struct Frame {
    kind: u8,
    flags: u8,
    stream: u32,
    payload: Vec<u8>,
}

struct Client {
    io: Tls,
    frames: Vec<Frame>,
}

fn frame(kind: u8, flags: u8, stream: u32, payload: &[u8]) -> Vec<u8> {
    let length = payload.len() as u32;
    let mut bytes = vec![
        (length >> 16) as u8,
        (length >> 8) as u8,
        length as u8,
        kind,
        flags,
    ];
    bytes.extend_from_slice(&stream.to_be_bytes());
    bytes.extend_from_slice(payload);
    bytes
}

impl Client {
    async fn connect(address: SocketAddr) -> Self {
        let io = wire_support::tls_connect(
            address,
            wire_support::PROXY_SERVER_NAME,
            "ca-a.pem",
            &[b"h2"],
        )
        .await
        .expect("TLS connection");
        let mut client = Self {
            io,
            frames: Vec::new(),
        };
        client
            .send(&[PREFACE, &frame(SETTINGS, 0, 0, &[])].concat())
            .await;
        client
    }

    async fn send(&mut self, bytes: &[u8]) {
        self.io.write_all(bytes).await.expect("proxy alive");
    }

    async fn read_frame(&mut self) -> Frame {
        let mut head = [0; 9];
        timeout(STEP, self.io.read_exact(&mut head))
            .await
            .expect("frame in time")
            .expect("connection open");
        let length = usize::from(head[0]) << 16 | usize::from(head[1]) << 8 | usize::from(head[2]);
        let mut payload = vec![0; length];
        self.io.read_exact(&mut payload).await.expect("payload");
        if matches!(head[3], 3 | 7) {
            eprintln!(
                "frame {} on stream {}: {payload:?}",
                head[3],
                u32::from_be_bytes([head[5], head[6], head[7], head[8]])
            );
        }
        Frame {
            kind: head[3],
            flags: head[4],
            stream: u32::from_be_bytes([head[5] & 0x7f, head[6], head[7], head[8]]),
            payload,
        }
    }

    /// The windows the proxy advertises: its SETTINGS_INITIAL_WINDOW_SIZE and 65,535 plus its connection update,
    /// all of which precede the acknowledgement of this client's SETTINGS.
    async fn advertised(&mut self) -> (u32, u32) {
        let (mut stream, mut connection) = (65_535, 65_535);
        loop {
            let f = self.read_frame().await;
            match f.kind {
                SETTINGS if f.flags & 1 == 1 => return (stream, connection),
                SETTINGS => {
                    for e in f.payload.chunks(6).filter(|e| e[1] == 4) {
                        stream = u32::from_be_bytes(e[2..6].try_into().unwrap());
                    }
                }
                WINDOW_UPDATE if f.stream == 0 => {
                    connection += u32::from_be_bytes(f.payload[..4].try_into().unwrap());
                }
                _ => {}
            }
        }
    }

    /// POSTs `total` octets, as much as the proxy's credit allows at a time, and returns the `WINDOW_UPDATE`s that
    /// arrived from the first DATA frame to the response.
    async fn upload(&mut self, total: usize, windows: (u32, u32)) -> usize {
        let authority = wire_support::PROXY_SERVER_NAME.as_bytes();
        let mut block = vec![0x83, 0x87, 0x84, 0x01, authority.len() as u8];
        block.extend_from_slice(authority);
        self.send(&frame(HEADERS, 4, 1, &block)).await;
        let (mut stream_credit, mut connection_credit) =
            (i64::from(windows.0), i64::from(windows.1));
        let (mut sent, mut updates) = (0, 0);
        let chunk = vec![5; FRAME];
        while sent < total {
            let n = FRAME.min(total - sent);
            while stream_credit < n as i64 || connection_credit < n as i64 {
                let f = self.read_frame().await;
                if f.kind == WINDOW_UPDATE {
                    updates += 1;
                    let increment =
                        i64::from(u32::from_be_bytes(f.payload[..4].try_into().unwrap()));
                    if f.stream == 0 {
                        connection_credit += increment;
                    } else {
                        stream_credit += increment;
                    }
                }
            }
            sent += n;
            self.send(&frame(DATA, u8::from(sent == total), 1, &chunk[..n]))
                .await;
            stream_credit -= n as i64;
            connection_credit -= n as i64;
        }
        loop {
            let f = self.read_frame().await;
            match f.kind {
                WINDOW_UPDATE => updates += 1,
                HEADERS if f.stream == 1 => {
                    assert_eq!(
                        f.payload[0], 0x88,
                        "the origin's 200 reaches the client: {:?}",
                        f.payload
                    );
                    return updates;
                }
                _ => {}
            }
        }
    }
}

#[tokio::test]
async fn a_listener_advertises_one_and_eight_mib_unless_configured() {
    let proxy = start(None).await;
    let mut client = Client::connect(proxy.address).await;
    assert_eq!(client.advertised().await, (MIB as u32, 8 * MIB as u32));
    proxy.process.shutdown();
    proxy.origin.abort();
}

#[tokio::test]
async fn configured_windows_are_advertised() {
    let proxy = start(Some((262_144, 2 * MIB as u64))).await;
    let mut client = Client::connect(proxy.address).await;
    assert_eq!(client.advertised().await, (262_144, 2 * MIB as u32));
    proxy.process.shutdown();
    proxy.origin.abort();
}

#[tokio::test]
async fn an_upload_costs_about_two_window_updates_per_mib_and_sixty_four_at_the_old_windows() {
    for (windows, bound) in [(None, 0.0..=3.0), (Some((65_535, 65_535)), 60.0..=70.0)] {
        let proxy = start(windows).await;
        let mut client = Client::connect(proxy.address).await;
        let advertised = client.advertised().await;
        let updates = tokio::spawn(async move { client.upload(8 * MIB, advertised).await }).await;
        let Ok(updates) = updates else {
            panic!(
                "{windows:?}: upload failed; proxy stderr:\n{}",
                proxy.process.shutdown_output()
            );
        };
        let per_mib = updates as f64 / 8.0;
        eprintln!("{windows:?}: {updates} WINDOW_UPDATEs for 8 MiB, {per_mib} per MiB");
        assert!(
            bound.contains(&per_mib),
            "{windows:?}: {updates} WINDOW_UPDATEs for 8 MiB, {per_mib} per MiB"
        );
        proxy.process.shutdown();
        proxy.origin.abort();
    }
}
