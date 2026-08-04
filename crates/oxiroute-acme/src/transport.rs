use std::{
    io::{Read as _, Write as _},
    net::{TcpStream, ToSocketAddrs},
    time::Duration,
};

use http::Uri;
use openssl::ssl::{SslConnector, SslMethod, SslStream};

use crate::protocol::{
    AcmeTransport, HttpRequest, HttpResponse, MAX_ACME_BODY_BYTES, TransportError,
};

const MAX_RESPONSE_HEADERS_BYTES: usize = 128 * 1024;
const MAX_RESPONSE_BYTES: usize = MAX_RESPONSE_HEADERS_BYTES + MAX_ACME_BODY_BYTES;
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_IO_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransportConfig {
    pub connect_timeout: Duration,
    pub io_timeout: Duration,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            io_timeout: DEFAULT_IO_TIMEOUT,
        }
    }
}

/// Bounded HTTPS/HTTP-1 transport for production ACME requests.
///
/// Redirects are deliberately not followed here. The protocol layer validates every response URL
/// against its configured origin policy.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SystemAcmeTransport {
    config: TransportConfig,
}

impl SystemAcmeTransport {
    #[must_use]
    pub const fn new(config: TransportConfig) -> Self {
        Self { config }
    }
}

impl AcmeTransport for SystemAcmeTransport {
    fn request(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        let uri = request.url.parse::<Uri>().map_err(|_| TransportError)?;
        let scheme = uri.scheme_str().ok_or(TransportError)?;
        if scheme != "https" {
            return Err(TransportError);
        }
        let authority = uri.authority().ok_or(TransportError)?;
        let host = authority.host();
        if host.is_empty() {
            return Err(TransportError);
        }
        let port = authority.port_u16().unwrap_or(443);
        let socket = socket_address(host, port).ok_or(TransportError)?;
        let stream = TcpStream::connect_timeout(&socket, self.config.connect_timeout)
            .map_err(|_| TransportError)?;
        stream
            .set_read_timeout(Some(self.config.io_timeout))
            .map_err(|_| TransportError)?;
        stream
            .set_write_timeout(Some(self.config.io_timeout))
            .map_err(|_| TransportError)?;
        let connector = SslConnector::builder(SslMethod::tls())
            .map_err(|_| TransportError)?
            .build();
        let mut stream = connector
            .connect(host, stream)
            .map_err(|_| TransportError)?;
        write_request(
            &mut stream,
            &request,
            authority.as_str(),
            uri.path_and_query(),
        )?;
        let raw = read_response(&mut stream)?;
        parse_response(&request.url, &raw)
    }
}

fn socket_address(host: &str, port: u16) -> Option<std::net::SocketAddr> {
    let address = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    address.to_socket_addrs().ok()?.next()
}

fn write_request(
    stream: &mut SslStream<TcpStream>,
    request: &HttpRequest,
    authority: &str,
    path_and_query: Option<&http::uri::PathAndQuery>,
) -> Result<(), TransportError> {
    if request.body.len() > MAX_ACME_BODY_BYTES {
        return Err(TransportError);
    }
    let target = path_and_query.map_or("/", http::uri::PathAndQuery::as_str);
    let mut head = format!(
        "{} {target} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\nContent-Length: {}\r\n",
        request.method,
        request.body.len()
    );
    for (name, value) in &request.headers {
        if !valid_header_name(name) || !valid_header_value(value) {
            return Err(TransportError);
        }
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    stream
        .write_all(head.as_bytes())
        .and_then(|()| stream.write_all(&request.body))
        .map_err(|_| TransportError)
}

fn read_response(stream: &mut SslStream<TcpStream>) -> Result<Vec<u8>, TransportError> {
    let mut response = Vec::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = stream.read(&mut buffer).map_err(|_| TransportError)?;
        if read == 0 {
            break;
        }
        if response.len().saturating_add(read) > MAX_RESPONSE_BYTES {
            return Err(TransportError);
        }
        response.extend_from_slice(&buffer[..read]);
    }
    Ok(response)
}

fn parse_response(url: &str, raw: &[u8]) -> Result<HttpResponse, TransportError> {
    let header_end = find_bytes(raw, b"\r\n\r\n").ok_or(TransportError)?;
    if header_end > MAX_RESPONSE_HEADERS_BYTES {
        return Err(TransportError);
    }
    let headers = std::str::from_utf8(&raw[..header_end]).map_err(|_| TransportError)?;
    let mut lines = headers.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split_ascii_whitespace().nth(1))
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or(TransportError)?;
    let mut response_headers = std::collections::BTreeMap::new();
    let mut content_length = None;
    let mut chunked = false;
    for line in lines {
        let (name, value) = line.split_once(':').ok_or(TransportError)?;
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim();
        if !valid_header_name(&name) || !valid_header_value(value) {
            return Err(TransportError);
        }
        if name == "content-length" {
            content_length = Some(value.parse::<usize>().map_err(|_| TransportError)?);
        }
        if name == "transfer-encoding" && value.eq_ignore_ascii_case("chunked") {
            chunked = true;
        }
        response_headers.insert(name, value.to_owned());
    }
    let body = &raw[header_end + 4..];
    let body = if chunked {
        decode_chunked(body)?
    } else if let Some(length) = content_length {
        if length > MAX_ACME_BODY_BYTES || body.len() < length {
            return Err(TransportError);
        }
        body[..length].to_vec()
    } else {
        body.to_vec()
    };
    if body.len() > MAX_ACME_BODY_BYTES {
        return Err(TransportError);
    }
    Ok(HttpResponse {
        status,
        url: url.into(),
        headers: response_headers,
        body,
    })
}

fn decode_chunked(mut body: &[u8]) -> Result<Vec<u8>, TransportError> {
    let mut output = Vec::new();
    loop {
        let line_end = find_bytes(body, b"\r\n").ok_or(TransportError)?;
        let size = usize::from_str_radix(
            std::str::from_utf8(&body[..line_end])
                .map_err(|_| TransportError)?
                .split(';')
                .next()
                .ok_or(TransportError)?
                .trim(),
            16,
        )
        .map_err(|_| TransportError)?;
        body = &body[line_end + 2..];
        if size == 0 {
            return Ok(output);
        }
        if size > MAX_ACME_BODY_BYTES || output.len().saturating_add(size) > MAX_ACME_BODY_BYTES {
            return Err(TransportError);
        }
        if body.len() < size + 2 || &body[size..size + 2] != b"\r\n" {
            return Err(TransportError);
        }
        output.extend_from_slice(&body[..size]);
        body = &body[size + 2..];
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn valid_header_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn valid_header_value(value: &str) -> bool {
    value.is_ascii() && !value.bytes().any(|byte| byte == b'\r' || byte == b'\n')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bounded_content_length_response() {
        let response = parse_response(
            "https://acme.test/directory",
            b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nReplay-Nonce: n1\r\n\r\nbody123",
        )
        .expect("response");
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"body123");
        assert_eq!(response.headers.get("replay-nonce"), Some(&"n1".into()));
    }

    #[test]
    fn decodes_chunked_response_with_extensions() {
        let response = parse_response(
            "https://acme.test/order",
            b"HTTP/1.1 201 Created\r\nTransfer-Encoding: chunked\r\n\r\n4;first\r\ntest\r\n0\r\n\r\n",
        )
        .expect("response");
        assert_eq!(response.status, 201);
        assert_eq!(response.body, b"test");
    }

    #[test]
    fn rejects_oversized_content_length_before_body_use() {
        let response = parse_response(
            "https://acme.test/order",
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                MAX_ACME_BODY_BYTES + 1
            )
            .as_bytes(),
        );
        assert!(response.is_err());
    }
}
