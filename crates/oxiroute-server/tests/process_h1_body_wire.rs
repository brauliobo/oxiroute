#![allow(dead_code, unused_imports, clippy::duplicate_mod)]
//! Requests with a body, and upgraded connections, on the proxied HTTP/1.1 path as seen on the wire: both framings
//! arrive intact, the response can flow while the upload is unfinished, an early or failed answer ends the
//! exchange, an aborted upload reaches the origin as an abort, and a stalled origin cannot make the proxy buffer an
//! upload without bound. The proxy is the real binary, so every hook runs.

#[path = "support/config.rs"]
mod config_support;
#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/h1_wire.rs"]
mod h1_wire;
#[path = "support/process.rs"]
mod process_support;

use std::{fs, net::SocketAddr, time::Duration};

use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    sync::oneshot,
    time::{sleep, timeout},
};

use h1_wire::{Captured, Origin, STEP, Wire, find, read_request, status};

const OK: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";

fn patterned(length: usize) -> Vec<u8> {
    (0..length)
        .map(|index| u8::try_from((index * 7 + index / 251) % 251).expect("below 251"))
        .collect()
}

fn post(length: usize) -> String {
    format!("POST /up HTTP/1.1\r\nHost: example.test\r\nContent-Length: {length}\r\n\r\n")
}

fn chunked_post() -> &'static str {
    "POST /up HTTP/1.1\r\nHost: example.test\r\nTransfer-Encoding: chunked\r\n\r\n"
}

/// The head of one response and its body, framed by `Content-Length` or `chunked`.
async fn read_response(stream: &mut TcpStream) -> (String, Vec<u8>) {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8192];
    let head_end = loop {
        if let Some(position) = find(&bytes, b"\r\n\r\n") {
            break position + 4;
        }
        let count = timeout(STEP, stream.read(&mut buffer))
            .await
            .expect("response head")
            .expect("read");
        assert_ne!(count, 0, "closed before the head: {bytes:?}");
        bytes.extend_from_slice(&buffer[..count]);
    };
    let head = String::from_utf8_lossy(&bytes[..head_end]).into_owned();
    let lower = head.to_ascii_lowercase();
    let length = lower
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|value| value.trim().parse::<usize>().ok());
    let chunked = lower.contains("transfer-encoding: chunked");
    loop {
        let complete = if chunked {
            bytes[head_end..].ends_with(b"0\r\n\r\n")
        } else {
            bytes.len() >= head_end + length.unwrap_or(0)
        };
        if complete {
            return (head, bytes[head_end..].to_vec());
        }
        let count = timeout(STEP, stream.read(&mut buffer))
            .await
            .expect("response body")
            .expect("read");
        assert_ne!(count, 0, "closed inside the body");
        bytes.extend_from_slice(&buffer[..count]);
    }
}

fn big_bodies(config: &mut oxiroute_config::ConfigDraft) {
    config.http_services[0].max_request_body_bytes = Some(64 * 1024 * 1024);
}

fn request_body(request: &[u8]) -> &[u8] {
    let head_end = find(request, b"\r\n\r\n").expect("request head") + 4;
    &request[head_end..]
}

/// Decodes a chunked body that ends with the last chunk.
fn dechunk(mut raw: &[u8]) -> Vec<u8> {
    let mut decoded = Vec::new();
    loop {
        let line_end = find(raw, b"\r\n").expect("chunk size line");
        let size = usize::from_str_radix(std::str::from_utf8(&raw[..line_end]).expect("size"), 16)
            .expect("hex size");
        if size == 0 {
            return decoded;
        }
        decoded.extend_from_slice(&raw[line_end + 2..line_end + 2 + size]);
        raw = &raw[line_end + 2 + size + 2..];
    }
}

#[tokio::test]
async fn bodies_of_both_framings_arrive_intact_on_one_keep_alive_connection() {
    let origin = Origin::start(OK).await;
    let wire = Wire::over(origin, big_bodies).await;
    let mut client = TcpStream::connect(wire.proxy).await.expect("connect");

    let large = patterned(300_000);
    client
        .write_all(post(large.len()).as_bytes())
        .await
        .unwrap();
    client.write_all(&large).await.unwrap();
    assert_eq!(read_response(&mut client).await.1, b"ok");

    client.write_all(chunked_post().as_bytes()).await.unwrap();
    let parts = [patterned(10), patterned(70_000), patterned(1)];
    for part in &parts {
        client
            .write_all(format!("{:x}\r\n", part.len()).as_bytes())
            .await
            .unwrap();
        client.write_all(part).await.unwrap();
        client.write_all(b"\r\n").await.unwrap();
        sleep(Duration::from_millis(20)).await;
    }
    client.write_all(b"0\r\n\r\n").await.unwrap();
    assert_eq!(read_response(&mut client).await.1, b"ok");

    client.write_all(post(0).as_bytes()).await.unwrap();
    assert_eq!(read_response(&mut client).await.1, b"ok");

    let requests = wire.origin.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 3);
    assert_eq!(request_body(&requests[0]), large);
    assert_eq!(dechunk(request_body(&requests[1])), parts.concat());
    assert_eq!(request_body(&requests[2]), b"");
    wire.finish();
}

/// An origin that answers a chunked response at once and echoes every request byte it reads as a response chunk.
async fn echoing_origin() -> Origin {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
    let address = listener.local_addr().expect("origin address");
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.expect("origin accept");
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut buffer = [0_u8; 4096];
                let mut answered = false;
                loop {
                    let Ok(count) = stream.read(&mut buffer).await else {
                        return;
                    };
                    if count == 0 {
                        return;
                    }
                    let data = if answered {
                        buffer[..count].to_vec()
                    } else {
                        head.extend_from_slice(&buffer[..count]);
                        let Some(head_end) = find(&head, b"\r\n\r\n") else {
                            continue;
                        };
                        answered = true;
                        let answer = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n";
                        stream.write_all(answer).await.expect("echo head");
                        head.split_off(head_end + 4)
                    };
                    if !data.is_empty() {
                        let framed = [
                            format!("{:x}\r\n", data.len()).into_bytes(),
                            data.clone(),
                            b"\r\n".to_vec(),
                        ];
                        stream
                            .write_all(&framed.concat())
                            .await
                            .expect("echo chunk");
                    }
                    if data.ends_with(b"0\r\n\r\n") {
                        stream.write_all(b"0\r\n\r\n").await.expect("echo end");
                    }
                }
            });
        }
    });
    Origin {
        address,
        requests: Captured::default(),
    }
}

#[tokio::test]
async fn the_response_flows_back_while_the_upload_is_still_open() {
    let wire = Wire::over(echoing_origin().await, |_| {}).await;
    let mut client = TcpStream::connect(wire.proxy).await.expect("connect");
    client.write_all(chunked_post().as_bytes()).await.unwrap();
    let mut echoed = Vec::new();
    let mut buffer = [0_u8; 4096];
    for round in 0..3_u8 {
        let part = [b'a' + round; 8];
        client.write_all(b"8\r\n").await.unwrap();
        client.write_all(&part).await.unwrap();
        client.write_all(b"\r\n").await.unwrap();
        // The origin echoes the framed chunk; the proxy must hand it to the client before the upload ends.
        while !String::from_utf8_lossy(&echoed).contains(std::str::from_utf8(&part).unwrap()) {
            let count = timeout(STEP, client.read(&mut buffer))
                .await
                .expect("the echo of an open upload")
                .unwrap();
            assert_ne!(count, 0);
            echoed.extend_from_slice(&buffer[..count]);
        }
    }
    client.write_all(b"0\r\n\r\n").await.unwrap();
    while !echoed.ends_with(b"0\r\n\r\n") {
        let count = timeout(STEP, client.read(&mut buffer))
            .await
            .expect("end")
            .unwrap();
        assert_ne!(count, 0);
        echoed.extend_from_slice(&buffer[..count]);
    }
    wire.finish();
}

#[tokio::test]
async fn an_early_final_response_reaches_the_client_while_the_upload_is_unfinished() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("origin accept");
        let mut buffer = [0_u8; 4096];
        let mut seen = Vec::new();
        while find(&seen, b"\r\n\r\n").is_none() {
            let count = stream.read(&mut buffer).await.unwrap();
            seen.extend_from_slice(&buffer[..count]);
        }
        stream
            .write_all(
                b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        // Keep the connection open and unread, as an origin that answers before it reads does.
        sleep(Duration::from_secs(10)).await;
    });
    let wire = Wire::over(
        Origin {
            address,
            requests: Captured::default(),
        },
        big_bodies,
    )
    .await;
    let mut client = TcpStream::connect(wire.proxy).await.expect("connect");
    client.write_all(post(5_000_000).as_bytes()).await.unwrap();
    client.write_all(&patterned(1000)).await.unwrap();
    let (head, _) = read_response(&mut client).await;
    assert_eq!(status(&head), "HTTP/1.1 401 Unauthorized");
    wire.finish();
}

#[tokio::test]
async fn an_origin_that_closes_inside_the_request_body_gives_a_bad_gateway() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("origin accept");
        let mut buffer = [0_u8; 4096];
        let mut seen = 0;
        while seen < 6000 {
            seen += stream.read(&mut buffer).await.unwrap();
        }
    });
    let wire = Wire::over(
        Origin {
            address,
            requests: Captured::default(),
        },
        big_bodies,
    )
    .await;
    let mut client = TcpStream::connect(wire.proxy).await.expect("connect");
    client.write_all(post(300_000).as_bytes()).await.unwrap();
    let body = patterned(300_000);
    // The write may fail once the proxy gave up on the upload; the answer is what counts.
    let _ = client.write_all(&body).await;
    let (head, _) = read_response(&mut client).await;
    assert_eq!(status(&head), "HTTP/1.1 502 Bad Gateway");
    wire.finish();
}

#[tokio::test]
async fn a_client_that_aborts_the_upload_aborts_the_origin_request() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
    let address = listener.local_addr().unwrap();
    let (aborted, observed) = oneshot::channel();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("origin accept");
        let mut buffer = [0_u8; 4096];
        let mut total = 0;
        loop {
            match stream.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(count) => total += count,
            }
        }
        let _ = aborted.send(total);
    });
    let wire = Wire::over(
        Origin {
            address,
            requests: Captured::default(),
        },
        big_bodies,
    )
    .await;
    let mut client = TcpStream::connect(wire.proxy).await.expect("connect");
    client.write_all(post(1_000_000).as_bytes()).await.unwrap();
    client.write_all(&patterned(5000)).await.unwrap();
    sleep(Duration::from_millis(200)).await;
    drop(client);
    let total = timeout(STEP, observed)
        .await
        .expect("origin sees the abort")
        .unwrap();
    assert!(total < 1_000_000, "{total}");
    wire.finish();
}

#[tokio::test]
async fn an_interim_response_is_relayed_before_the_client_sends_the_body() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
    let address = listener.local_addr().unwrap();
    let origin_body = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = std::sync::Arc::clone(&origin_body);
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("origin accept");
        let mut buffer = [0_u8; 4096];
        let mut head = Vec::new();
        while find(&head, b"\r\n\r\n").is_none() {
            let count = stream.read(&mut buffer).await.unwrap();
            head.extend_from_slice(&buffer[..count]);
        }
        stream
            .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
            .await
            .unwrap();
        let mut body = head.split_off(find(&head, b"\r\n\r\n").unwrap() + 4);
        while body.len() < 11 {
            let count = stream.read(&mut buffer).await.unwrap();
            body.extend_from_slice(&buffer[..count]);
        }
        *seen.lock().unwrap() = body;
        stream.write_all(OK).await.unwrap();
    });
    let wire = Wire::over(
        Origin {
            address,
            requests: Captured::default(),
        },
        |_| {},
    )
    .await;
    let mut client = TcpStream::connect(wire.proxy).await.expect("connect");
    client
        .write_all(b"POST /up HTTP/1.1\r\nHost: example.test\r\nExpect: 100-continue\r\nContent-Length: 11\r\n\r\n")
        .await
        .unwrap();
    let mut interim = Vec::new();
    while find(&interim, b"\r\n\r\n").is_none() {
        let mut byte = [0_u8; 1];
        timeout(STEP, client.read_exact(&mut byte))
            .await
            .expect("100 Continue")
            .unwrap();
        interim.push(byte[0]);
    }
    assert_eq!(
        status(&String::from_utf8_lossy(&interim)),
        "HTTP/1.1 100 Continue"
    );
    client.write_all(b"hello world").await.unwrap();
    let (head, body) = read_response(&mut client).await;
    assert_eq!(status(&head), "HTTP/1.1 200 OK");
    assert_eq!(body, b"ok");
    assert_eq!(origin_body.lock().unwrap().as_slice(), b"hello world");
    wire.finish();
}

#[tokio::test]
async fn a_body_over_the_service_limit_is_refused_and_never_reaches_the_origin() {
    let wire = Wire::start(OK).await;
    let mut client = TcpStream::connect(wire.proxy).await.expect("connect");
    client.write_all(post(100_000).as_bytes()).await.unwrap();
    let _ = client.write_all(&patterned(100_000)).await;
    let (head, _) = read_response(&mut client).await;
    assert_eq!(status(&head), "HTTP/1.1 413 Payload Too Large");
    wire.finish();
}

fn resident_kib(pid: u32) -> u64 {
    fs::read_to_string(format!("/proc/{pid}/status"))
        .expect("proc status")
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|rest| rest.trim().trim_end_matches("kB").trim().parse().ok())
        .expect("VmRSS")
}

#[tokio::test]
async fn a_stalled_origin_does_not_make_the_proxy_buffer_the_upload() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("origin accept");
        sleep(Duration::from_secs(20)).await;
        drop(stream);
    });
    let wire = Wire::over(
        Origin {
            address,
            requests: Captured::default(),
        },
        big_bodies,
    )
    .await;
    let before = resident_kib(wire.server.pid());
    let mut client = TcpStream::connect(wire.proxy).await.expect("connect");
    let total = 64 * 1024 * 1024;
    client.write_all(post(total).as_bytes()).await.unwrap();
    let block = patterned(1024 * 1024);
    let mut sent = 0;
    // The origin never reads, so the client must be held back long before the whole body is written.
    while sent < total
        && timeout(Duration::from_millis(700), client.write_all(&block))
            .await
            .is_ok()
    {
        sent += block.len();
    }
    assert!(sent < total, "the proxy took the whole {total} byte upload");
    let growth = resident_kib(wire.server.pid()).saturating_sub(before);
    assert!(
        growth < 24 * 1024,
        "the proxy grew by {growth} KiB for a stalled upload"
    );
    wire.finish();
}

#[tokio::test]
async fn an_upgraded_connection_echoes_frames_of_every_size_and_ends_on_a_half_close() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("origin accept");
        let mut buffer = [0_u8; 16384];
        let mut head = Vec::new();
        while find(&head, b"\r\n\r\n").is_none() {
            let count = stream.read(&mut buffer).await.unwrap();
            head.extend_from_slice(&buffer[..count]);
        }
        stream
            .write_all(b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n")
            .await
            .unwrap();
        loop {
            match stream.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(count) => stream.write_all(&buffer[..count]).await.unwrap(),
            }
        }
        let _ = stream.shutdown().await;
    });
    let wire = Wire::over(
        Origin {
            address,
            requests: Captured::default(),
        },
        |_| {},
    )
    .await;
    let mut client = TcpStream::connect(wire.proxy).await.expect("connect");
    client
        .write_all(
            b"GET /ws HTTP/1.1\r\nHost: example.test\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\
              Sec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
        )
        .await
        .unwrap();
    let mut head = vec![0_u8; 1];
    let mut got = Vec::new();
    while find(&got, b"\r\n\r\n").is_none() {
        let count = timeout(STEP, client.read(&mut head))
            .await
            .expect("101")
            .unwrap();
        assert_ne!(count, 0);
        got.extend_from_slice(&head[..count]);
    }
    assert_eq!(
        status(&String::from_utf8_lossy(&got)),
        "HTTP/1.1 101 Switching Protocols"
    );
    for size in [1_usize, 2, 125, 126, 1024, 4096, 65_535, 70_000, 300_000] {
        let frame = patterned(size);
        client.write_all(&frame).await.unwrap();
        let mut echo = vec![0_u8; size];
        timeout(STEP, client.read_exact(&mut echo))
            .await
            .expect("echo")
            .unwrap();
        assert_eq!(echo, frame, "frame of {size} bytes");
    }
    client.shutdown().await.unwrap();
    let mut rest = Vec::new();
    timeout(STEP, client.read_to_end(&mut rest))
        .await
        .expect("close after the half close")
        .unwrap();
    assert!(rest.is_empty());
    wire.finish();
}
