#![allow(dead_code)]

use std::{fmt::Write as _, net::SocketAddr};

use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpStream,
};

pub async fn open_event_stream(
    address: SocketAddr,
    authorization: &str,
    last_event_id: Option<u64>,
) -> (TcpStream, Vec<u8>) {
    let mut request = format!(
        "GET /api/v1/events/stream HTTP/1.1\r\nHost: localhost\r\nAuthorization: {authorization}\r\nAccept: text/event-stream\r\nConnection: close\r\n"
    );
    if let Some(last_event_id) = last_event_id {
        write!(request, "Last-Event-ID: {last_event_id}\r\n").expect("write SSE Last-Event-ID");
    }
    request.push_str("\r\n");

    let mut stream = TcpStream::connect(address).await.expect("SSE connection");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("SSE request");
    let mut head = Vec::new();
    let mut byte = [0_u8; 1];
    while !head.windows(4).any(|window| window == b"\r\n\r\n") {
        stream
            .read_exact(&mut byte)
            .await
            .expect("SSE response headers");
        head.push(byte[0]);
    }
    (stream, head)
}

pub async fn read_chunk(stream: &mut TcpStream) -> Vec<u8> {
    let mut line = Vec::new();
    let mut byte = [0_u8; 1];
    while !line.ends_with(b"\r\n") {
        stream.read_exact(&mut byte).await.expect("SSE chunk size");
        line.push(byte[0]);
    }
    let size = usize::from_str_radix(
        std::str::from_utf8(&line[..line.len() - 2]).expect("SSE chunk size UTF-8"),
        16,
    )
    .expect("SSE chunk size");
    let mut body = vec![0_u8; size];
    stream.read_exact(&mut body).await.expect("SSE chunk body");
    let mut terminator = [0_u8; 2];
    stream
        .read_exact(&mut terminator)
        .await
        .expect("SSE chunk terminator");
    assert_eq!(terminator, *b"\r\n");
    body
}
