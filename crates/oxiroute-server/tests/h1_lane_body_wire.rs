#![cfg(unix)]
#![allow(dead_code, unused_imports, clippy::duplicate_mod)]
//! Request bodies that arrive after the head on the HTTP/1.1 lane. The lane streams them to the origin as they come and
//! watches the origin's answer meanwhile; the real binary says which requests it served, so a case cannot pass because
//! the classic path took the request. The classic path's own behaviour for the same bodies is pinned by
//! `process_h1_body_wire` (which runs on whichever path serves the request) and by the corpus of `lane_differential`.

#[path = "support/config.rs"]
mod config_support;
#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/h1_wire.rs"]
mod h1_wire;
#[path = "support/process.rs"]
mod process_support;

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    time::{sleep, timeout},
};

use h1_wire::{Captured, Origin, STEP, Wire, find, read_request, status};

const SERVED: &str = "served by the HTTP/1 lane";
const OK: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";

fn patterned(length: usize) -> Vec<u8> {
    (0..length)
        .map(|index| u8::try_from((index * 7 + index / 251) % 251).expect("below 251"))
        .collect()
}

fn post(length: usize) -> String {
    format!("POST /up HTTP/1.1\r\nHost: example.test\r\nContent-Length: {length}\r\n\r\n")
}

fn big_bodies(config: &mut oxiroute_config::ConfigDraft) {
    config.http_services[0].max_request_body_bytes = Some(64 * 1024 * 1024);
    config.log_filter = Some("oxiroute::lane=debug".into());
}

/// Stops the proxy; the number of requests it served by the lane.
fn served(wire: Wire) -> usize {
    let output = wire.server.shutdown_output();
    output.lines().filter(|line| line.contains(SERVED)).count()
}

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
    let length = head
        .to_ascii_lowercase()
        .lines()
        .find_map(|line| line.strip_prefix("content-length:").map(str::to_owned))
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    while bytes.len() < head_end + length {
        let count = timeout(STEP, stream.read(&mut buffer))
            .await
            .expect("response body")
            .expect("read");
        assert_ne!(count, 0, "closed inside the body");
        bytes.extend_from_slice(&buffer[..count]);
    }
    (head, bytes[head_end..].to_vec())
}

fn request_body(request: &[u8]) -> &[u8] {
    let head_end = find(request, b"\r\n\r\n").expect("request head") + 4;
    &request[head_end..]
}

#[tokio::test]
async fn bodies_that_arrive_after_the_head_are_served_by_the_lane_and_arrive_intact() {
    let wire = Wire::over(Origin::start(OK).await, big_bodies).await;
    let sizes = [1, 8000, 8192, 8193, 20_000, 65_536, 300_000];
    let mut client = TcpStream::connect(wire.proxy).await.expect("connect");
    for size in sizes {
        let body = patterned(size);
        client.write_all(post(size).as_bytes()).await.unwrap();
        client.write_all(&body).await.unwrap();
        let (head, answer) = read_response(&mut client).await;
        assert_eq!(status(&head), "HTTP/1.1 200 OK", "size {size}");
        assert_eq!(answer, b"ok");
    }
    let received: Vec<Vec<u8>> = wire
        .origin
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|request| request_body(request).to_vec())
        .collect();
    for (size, body) in sizes.into_iter().zip(received) {
        assert_eq!(body, patterned(size), "the body of {size} bytes");
    }
    assert_eq!(served(wire), sizes.len());
}

#[tokio::test]
async fn a_slow_uploader_is_forwarded_as_it_arrives_and_the_connection_goes_on() {
    let wire = Wire::over(Origin::start(OK).await, big_bodies).await;
    let mut client = TcpStream::connect(wire.proxy).await.expect("connect");
    client.write_all(post(30_000).as_bytes()).await.unwrap();
    for _ in 0..3_usize {
        client.write_all(&patterned(10_000)).await.unwrap();
        sleep(Duration::from_millis(100)).await;
    }
    let (head, _) = read_response(&mut client).await;
    assert_eq!(status(&head), "HTTP/1.1 200 OK");
    client.write_all(post(3).as_bytes()).await.unwrap();
    client.write_all(b"abc").await.unwrap();
    let (head, _) = read_response(&mut client).await;
    assert_eq!(status(&head), "HTTP/1.1 200 OK");
    assert_eq!(served(wire), 2);
}

/// The classic path answers `400` here and loses the first answer, so the corpus of `lane_differential` cannot hold it.
#[tokio::test]
async fn a_request_pipelined_after_a_streamed_body_is_served_next() {
    let wire = Wire::over(Origin::start(OK).await, big_bodies).await;
    let mut client = TcpStream::connect(wire.proxy).await.expect("connect");
    client.write_all(post(10).as_bytes()).await.unwrap();
    client.write_all(b"01234").await.unwrap();
    sleep(Duration::from_millis(100)).await;
    let next = "GET /next HTTP/1.1\r\nHost: example.test\r\n\r\n";
    client
        .write_all(&[b"56789".as_slice(), next.as_bytes()].concat())
        .await
        .unwrap();
    for _ in 0..2_usize {
        let (head, answer) = read_response(&mut client).await;
        assert_eq!(status(&head), "HTTP/1.1 200 OK");
        assert_eq!(answer, b"ok");
    }
    let requests = wire.origin.requests.lock().unwrap().len();
    assert_eq!(requests, 2);
    assert_eq!(served(wire), 2);
}

/// An origin that records the bytes of the body as they arrive, in `seen`, and answers once `length` bytes are there.
async fn recording_origin(length: usize, seen: Arc<Mutex<usize>>) -> Origin {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("origin accept");
        let mut buffer = [0_u8; 4096];
        let mut head = Vec::new();
        while find(&head, b"\r\n\r\n").is_none() {
            let count = stream.read(&mut buffer).await.unwrap();
            head.extend_from_slice(&buffer[..count]);
        }
        let mut total = head.len() - (find(&head, b"\r\n\r\n").unwrap() + 4);
        *seen.lock().unwrap() = total;
        while total < length {
            let count = stream.read(&mut buffer).await.unwrap();
            total += count;
            *seen.lock().unwrap() = total;
        }
        stream.write_all(OK).await.unwrap();
        sleep(Duration::from_secs(5)).await;
    });
    Origin {
        address,
        requests: Captured::default(),
    }
}

#[tokio::test]
async fn the_origin_sees_each_piece_before_the_client_sends_the_next() {
    let seen = Arc::new(Mutex::new(0_usize));
    let wire = Wire::over(
        recording_origin(30_000, Arc::clone(&seen)).await,
        big_bodies,
    )
    .await;
    let mut client = TcpStream::connect(wire.proxy).await.expect("connect");
    client.write_all(post(30_000).as_bytes()).await.unwrap();
    for round in 1..=3_usize {
        client.write_all(&patterned(10_000)).await.unwrap();
        timeout(STEP, async {
            while *seen.lock().unwrap() < 10_000 * round {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the piece reached the origin before the next one was sent");
    }
    let (head, _) = read_response(&mut client).await;
    assert_eq!(status(&head), "HTTP/1.1 200 OK");
    assert_eq!(served(wire), 1);
}

#[tokio::test]
async fn an_origin_that_answers_before_the_upload_ends_gets_no_more_and_the_connection_closes() {
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
            .write_all(b"HTTP/1.1 413 Payload Too Large\r\nContent-Length: 0\r\n\r\n")
            .await
            .unwrap();
        sleep(Duration::from_secs(10)).await;
    });
    let origin = Origin {
        address,
        requests: Captured::default(),
    };
    let wire = Wire::over(origin, big_bodies).await;
    let mut client = TcpStream::connect(wire.proxy).await.expect("connect");
    client.write_all(post(5_000_000).as_bytes()).await.unwrap();
    client.write_all(&patterned(1000)).await.unwrap();
    let (head, _) = read_response(&mut client).await;
    assert_eq!(status(&head), "HTTP/1.1 413 Payload Too Large");
    assert!(
        !head.to_ascii_lowercase().contains("connection: keep-alive"),
        "the rest of the body is still to come: {head}"
    );
    let mut rest = Vec::new();
    let end = timeout(STEP, client.read_to_end(&mut rest)).await;
    assert!(end.is_ok(), "the proxy closes the connection");
    assert_eq!(served(wire), 1);
}

#[tokio::test]
async fn a_client_that_aborts_the_upload_aborts_the_origin_request() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
    let address = listener.local_addr().unwrap();
    let (aborted, observed) = tokio::sync::oneshot::channel();
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
    let origin = Origin {
        address,
        requests: Captured::default(),
    };
    let wire = Wire::over(origin, big_bodies).await;
    let mut client = TcpStream::connect(wire.proxy).await.expect("connect");
    client.write_all(post(1_000_000).as_bytes()).await.unwrap();
    client.write_all(&patterned(20_000)).await.unwrap();
    sleep(Duration::from_millis(200)).await;
    drop(client);
    let total = timeout(STEP, observed)
        .await
        .expect("the origin sees the abort")
        .unwrap();
    assert!(total < 1_000_000, "{total}");
    // The proxy goes on.
    let mut client = TcpStream::connect(wire.proxy).await.expect("connect");
    client.write_all(post(0).as_bytes()).await.unwrap();
    assert_eq!(served(wire), 2);
}

#[tokio::test]
async fn a_pipelined_request_after_the_body_is_served_after_it() {
    let wire = Wire::over(Origin::start(OK).await, big_bodies).await;
    let mut client = TcpStream::connect(wire.proxy).await.expect("connect");
    client.write_all(post(20_000).as_bytes()).await.unwrap();
    client.write_all(&patterned(5000)).await.unwrap();
    sleep(Duration::from_millis(50)).await;
    let mut last = patterned(15_000);
    last.extend_from_slice(b"GET /next HTTP/1.1\r\nHost: example.test\r\n\r\n");
    client.write_all(&last).await.unwrap();
    for _ in 0..2 {
        let (head, answer) = read_response(&mut client).await;
        assert_eq!(status(&head), "HTTP/1.1 200 OK");
        assert_eq!(answer, b"ok");
    }
    let requests = wire.origin.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].starts_with("GET /next"));
    assert_eq!(served(wire), 2);
}
