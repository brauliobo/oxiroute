#![allow(dead_code)]

use std::{collections::HashMap, fmt::Write as _, io, net::SocketAddr, time::Duration};

use serde_json::Value;
use tokio::{
    io::{AsyncRead, AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpStream,
    time::timeout,
};

const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

pub struct HttpResponse {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
    raw: Vec<u8>,
}

impl HttpResponse {
    pub fn parse(raw: Vec<u8>) -> Self {
        let header_end = raw
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("HTTP response headers");
        let head = std::str::from_utf8(&raw[..header_end]).expect("UTF-8 HTTP headers");
        let mut lines = head.split("\r\n");
        let status = lines
            .next()
            .and_then(|line| line.split_ascii_whitespace().nth(1))
            .and_then(|value| value.parse().ok())
            .expect("HTTP response status");
        let headers = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
            .collect::<HashMap<_, _>>();
        let mut body = raw[header_end + 4..].to_vec();
        if headers
            .get("transfer-encoding")
            .is_some_and(|value| value.eq_ignore_ascii_case("chunked"))
        {
            body = decode_chunked(&body);
        } else if let Some(length) = headers
            .get("content-length")
            .and_then(|value| value.parse::<usize>().ok())
            && body.len() >= length
        {
            body.truncate(length);
        }
        Self {
            status,
            headers,
            body,
            raw,
        }
    }

    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).expect("HTTP JSON body")
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.raw).into_owned()
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }

    pub fn body(&self) -> &[u8] {
        &self.body
    }
}

pub async fn http_request(
    address: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> HttpResponse {
    let mut request =
        format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n");
    for (name, value) in headers {
        writeln!(request, "{name}: {value}\r").expect("write HTTP request header");
    }
    if !body.is_empty() || matches!(method, "POST" | "PUT") {
        write!(request, "Content-Length: {}\r\n", body.len())
            .expect("write request content length");
    }
    request.push_str("\r\n");
    let mut request = request.into_bytes();
    request.extend_from_slice(body);
    raw_http_request(address, &request).await
}

pub async fn raw_http_request(address: SocketAddr, request: &[u8]) -> HttpResponse {
    timeout(HTTP_TIMEOUT, async {
        let mut stream = TcpStream::connect(address)
            .await
            .expect("connect to server HTTP endpoint");
        stream.write_all(request).await.expect("write HTTP request");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .expect("read HTTP response");
        HttpResponse::parse(response)
    })
    .await
    .expect("HTTP exchange timed out")
}

pub async fn read_request_head<S>(stream: &mut S) -> io::Result<Vec<u8>>
where
    S: AsyncRead + Unpin,
{
    let mut request = Vec::new();
    let mut buffer = [0; 1024];
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = stream.read(&mut buffer).await?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "request ended before its headers",
            ));
        }
        request.extend_from_slice(&buffer[..read]);
    }
    Ok(request)
}

fn decode_chunked(bytes: &[u8]) -> Vec<u8> {
    let mut position = 0;
    let mut decoded = Vec::new();
    loop {
        let line_end = bytes[position..]
            .windows(2)
            .position(|window| window == b"\r\n")
            .map(|offset| position + offset)
            .expect("chunk size terminator");
        let size = usize::from_str_radix(
            std::str::from_utf8(&bytes[position..line_end])
                .expect("chunk size UTF-8")
                .split(';')
                .next()
                .expect("chunk size"),
            16,
        )
        .expect("chunk size hex");
        position = line_end + 2;
        if size == 0 {
            break;
        }
        decoded.extend_from_slice(&bytes[position..position + size]);
        position += size;
        assert_eq!(&bytes[position..position + 2], b"\r\n");
        position += 2;
    }
    decoded
}
