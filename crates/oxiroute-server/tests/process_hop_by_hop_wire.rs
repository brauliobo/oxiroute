#![allow(dead_code, unused_imports, clippy::duplicate_mod)]
//! Hop-by-hop fields of HTTP/1.1 proxying as seen on the wire (RFC 9110 section 7.6.1): what the origin receives
//! for a client request and what the client receives for an origin response. The proxy is the real binary, so
//! every hook runs.

#[path = "support/config.rs"]
mod config_support;
#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/h1_wire.rs"]
mod h1_wire;
#[path = "support/process.rs"]
mod process_support;

use std::time::Duration;

use oxiroute_config::{
    ConfigDraft, HttpProxyPolicy, HttpRequestHeaderMutation, HttpRequestHeaderValue,
    HttpRouteAction, HttpUpstreamHost,
};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpStream,
    time::timeout,
};

use h1_wire::{Origin, STEP, Wire, find, header_names, status};

const OK: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
const HOST: &str = "Host: example.test\r\n";

fn header_lines(head: &str, name: &str) -> Vec<String> {
    head.split("\r\n")
        .skip(1)
        .take_while(|line| !line.is_empty())
        .filter_map(|line| line.split_once(": "))
        .filter(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.to_owned())
        .collect()
}

fn has_header(head: &str, name: &str) -> bool {
    !header_lines(head, name).is_empty()
}

fn get(headers: &str) -> Vec<u8> {
    format!("GET /r HTTP/1.1\r\n{HOST}{headers}Connection: close\r\n\r\n").into_bytes()
}

/// The origin's view of one request sent with `headers` (a `Connection: close` of the client follows them).
async fn request_seen(headers: &str) -> String {
    let wire = Wire::start(OK).await;
    let response = wire.exchange(&get(headers)).await;
    assert_eq!(status(&response), "HTTP/1.1 200 OK", "{response}");
    let request = wire.origin.requests().remove(0);
    wire.finish();
    request
}

/// The client's view of the origin response `reply`.
async fn response_seen(reply: &'static [u8]) -> String {
    let wire = Wire::start(reply).await;
    let response = wire.exchange(&get("")).await;
    wire.finish();
    response
}

// Requests: what the origin receives.

#[tokio::test]
async fn nominated_and_hop_by_hop_request_headers_do_not_reach_the_origin() {
    let request = request_seen(concat!(
        "Connection: keep-alive, X-Nominated\r\nX-Nominated: secret\r\nTE: trailers\r\n",
        "Proxy-Authorization: Basic eA==\r\nProxy-Authenticate: Basic\r\nKeep-Alive: 5\r\n",
        "Proxy-Connection: keep-alive\r\nTrailer: X-Sum\r\nX-Keep: 1\r\nAuthorization: Bearer kept\r\n",
    ))
    .await;
    for name in [
        "x-nominated",
        "te",
        "proxy-authorization",
        "proxy-authenticate",
        "keep-alive",
        "proxy-connection",
        "trailer",
        "connection",
    ] {
        assert!(
            !has_header(&request, name),
            "{name} reached the origin: {request}"
        );
    }
    assert_eq!(header_lines(&request, "x-keep"), ["1"], "{request}");
    assert_eq!(
        header_lines(&request, "authorization"),
        ["Bearer kept"],
        "{request}"
    );
}

#[tokio::test]
async fn connection_options_are_matched_across_headers_case_whitespace_and_empty_elements() {
    let request = request_seen(concat!(
        "Connection: , X-One ,,\tx-Two,\r\nconnection:X-THREE\r\nCONNECTION: ,\r\n",
        "x-one: 1\r\nX-Two: 2\r\nX-Three: 3\r\nX-Four: 4\r\nX-Two: again\r\n",
    ))
    .await;
    for name in ["x-one", "x-two", "x-three", "connection"] {
        assert!(
            !has_header(&request, name),
            "{name} reached the origin: {request}"
        );
    }
    assert_eq!(header_lines(&request, "x-four"), ["4"], "{request}");
}

#[tokio::test]
async fn a_request_without_hop_by_hop_headers_is_forwarded_unchanged_in_order() {
    let wire = Wire::start(OK).await;
    wire.exchange(b"GET /r HTTP/1.1\r\nHost: example.test\r\nZ-Last: 1\r\nA-First: 2\r\n\r\n")
        .await;
    let request = wire.origin.requests().remove(0);
    let names = header_names(&request);
    let position = |name: &str| names.iter().position(|n| n.eq_ignore_ascii_case(name));
    assert!(position("host") < position("z-last"), "{request}");
    assert!(position("z-last") < position("a-first"), "{request}");
    wire.finish();
}

fn policy_edit(config: &mut ConfigDraft) {
    let HttpRouteAction::Proxy { policy, .. } = &mut config.http_services[0].routes[0].action
    else {
        unreachable!("the wire route proxies")
    };
    *policy = HttpProxyPolicy {
        upstream_host: HttpUpstreamHost::Literal {
            value: "origin.example:8080".into(),
        },
        request_headers: vec![
            HttpRequestHeaderMutation::Set {
                name: "x-forwarded-for".into(),
                value: HttpRequestHeaderValue::AppendedXForwardedFor {
                    max_bytes: 128,
                    except_source_cidrs: Vec::new(),
                },
            },
            HttpRequestHeaderMutation::Set {
                name: "x-real-ip".into(),
                value: HttpRequestHeaderValue::ClientIp,
            },
        ],
        ..HttpProxyPolicy::default()
    }
    .into();
}

#[tokio::test]
async fn a_client_cannot_nominate_the_headers_the_proxy_adds_for_removal() {
    let wire = Wire::over(Origin::start(OK).await, policy_edit).await;
    let response = wire
        .exchange(
            b"GET /r HTTP/1.1\r\nHost: example.test\r\nX-Forwarded-For: 198.51.100.4\r\nX-Request-Id: client-1\r\nX-Real-Ip: 198.51.100.9\r\nConnection: x-forwarded-for, x-request-id, x-real-ip, close\r\n\r\n",
        )
        .await;
    assert_eq!(status(&response), "HTTP/1.1 200 OK", "{response}");
    let request = wire.origin.requests().remove(0);
    assert_eq!(
        header_lines(&request, "x-forwarded-for"),
        ["127.0.0.1"],
        "{request}"
    );
    assert_eq!(
        header_lines(&request, "x-real-ip"),
        ["127.0.0.1"],
        "{request}"
    );
    assert_eq!(
        header_lines(&request, "host"),
        ["origin.example:8080"],
        "{request}"
    );
    let ids = header_lines(&request, "x-request-id");
    assert_eq!(ids.len(), 1, "{request}");
    assert_eq!(header_lines(&response, "x-request-id"), ids, "{response}");
    wire.finish();
}

#[tokio::test]
async fn nominating_a_framing_or_routing_header_is_refused_before_the_origin() {
    for nominated in [
        "Content-Length",
        "Transfer-Encoding",
        "Host",
        "x-fine, CONTENT-LENGTH",
    ] {
        let wire = Wire::start(OK).await;
        let request = format!(
            "POST /r HTTP/1.1\r\n{HOST}Content-Length: 5\r\nConnection: close, {nominated}\r\n\r\nhello"
        );
        let response = wire.exchange(request.as_bytes()).await;
        assert_eq!(
            status(&response),
            "HTTP/1.1 400 Bad Request",
            "{nominated}: {response}"
        );
        assert!(wire.origin.requests().is_empty(), "{nominated}");
        wire.finish();
    }
}

#[tokio::test]
async fn a_malformed_connection_option_is_refused_before_the_origin() {
    let wire = Wire::start(OK).await;
    let response = wire.exchange(&get("Connection: two words\r\n")).await;
    assert_eq!(status(&response), "HTTP/1.1 400 Bad Request", "{response}");
    assert!(wire.origin.requests().is_empty());
    wire.finish();
}

#[tokio::test]
async fn a_chunked_request_keeps_its_framing_while_its_connection_options_go() {
    let wire = Wire::start(OK).await;
    let response = wire
        .exchange(
            b"POST /r HTTP/1.1\r\nHost: example.test\r\nTransfer-Encoding: chunked\r\nConnection: close, x-gone\r\nX-Gone: 1\r\n\r\n5\r\nhello\r\n0\r\n\r\n",
        )
        .await;
    assert_eq!(status(&response), "HTTP/1.1 200 OK", "{response}");
    let request = wire.origin.requests().remove(0);
    assert_eq!(
        header_lines(&request, "transfer-encoding"),
        ["chunked"],
        "{request}"
    );
    assert!(!has_header(&request, "x-gone"), "{request}");
    assert!(request.ends_with("5\r\nhello\r\n0\r\n\r\n"), "{request}");
    wire.finish();
}

// Connection semantics that stay.

/// One complete response (head and `Content-Length` body) from `stream`.
async fn read_response(stream: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        if let Some(end) = find(&bytes, b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
            let length = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length: "))
                .map_or(0, |value| value.parse::<usize>().expect("length"));
            if bytes.len() >= end + 4 + length {
                return String::from_utf8_lossy(&bytes).into_owned();
            }
        }
        let count = timeout(STEP, stream.read(&mut buffer))
            .await
            .expect("response")
            .expect("read");
        assert!(count > 0, "closed after {} bytes", bytes.len());
        bytes.extend_from_slice(&buffer[..count]);
    }
}

#[tokio::test]
async fn keep_alive_clients_keep_their_connection_and_the_upstream_connection_is_reused() {
    let wire = Wire::start(OK).await;
    let mut stream = TcpStream::connect(wire.proxy).await.expect("connect");
    for index in 0..2 {
        let request = format!(
            "GET /{index} HTTP/1.1\r\n{HOST}Connection: keep-alive\r\nKeep-Alive: 5\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await.expect("write");
        let response = read_response(&mut stream).await;
        assert_eq!(status(&response), "HTTP/1.1 200 OK", "{response}");
    }
    let requests = wire.origin.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert!(
            !has_header(request, "connection") && !has_header(request, "keep-alive"),
            "{request}"
        );
    }
    wire.finish();
}

#[tokio::test]
async fn connection_close_ends_the_client_connection_and_is_not_sent_to_the_origin() {
    let wire = Wire::start(OK).await;
    let mut stream = TcpStream::connect(wire.proxy).await.expect("connect");
    let request = format!("GET /r HTTP/1.1\r\n{HOST}Connection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.expect("write");
    let response = read_response(&mut stream).await;
    assert_eq!(
        header_lines(&response, "connection"),
        ["close"],
        "{response}"
    );
    let mut rest = Vec::new();
    timeout(STEP, stream.read_to_end(&mut rest))
        .await
        .expect("closed")
        .expect("read");
    assert!(rest.is_empty());
    assert!(!has_header(&wire.origin.requests()[0], "connection"));
    wire.finish();
}

const HANDSHAKE: &str = concat!(
    "GET /ws HTTP/1.1\r\nHost: example.test\r\nUpgrade: websocket\r\n",
    "Connection: keep-alive, Upgrade, X-Gone\r\nX-Gone: 1\r\nKeep-Alive: 5\r\n",
    "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n",
);
const SWITCHED: &[u8] =
    b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n";

#[tokio::test]
async fn a_websocket_handshake_keeps_only_its_upgrade_options() {
    let wire = Wire::over(Origin::start(SWITCHED).await, |_| {}).await;
    let response = wire.exchange(HANDSHAKE.as_bytes()).await;
    assert_eq!(
        status(&response),
        "HTTP/1.1 101 Switching Protocols",
        "{response}"
    );
    assert_eq!(
        header_lines(&response, "upgrade"),
        ["websocket"],
        "{response}"
    );
    assert_eq!(header_lines(&response, "connection").len(), 1, "{response}");
    assert!(
        response
            .to_ascii_lowercase()
            .contains("connection: upgrade"),
        "{response}"
    );
    let request = wire.origin.requests().remove(0);
    assert_eq!(
        header_lines(&request, "upgrade"),
        ["websocket"],
        "{request}"
    );
    assert_eq!(
        header_lines(&request, "connection"),
        ["Upgrade"],
        "{request}"
    );
    assert_eq!(
        header_lines(&request, "sec-websocket-key"),
        ["dGhlIHNhbXBsZSBub25jZQ=="],
        "{request}"
    );
    assert!(
        !has_header(&request, "x-gone") && !has_header(&request, "keep-alive"),
        "{request}"
    );
    wire.finish();
}

#[tokio::test]
async fn upgrade_without_the_connection_option_is_not_forwarded() {
    let request = request_seen("Upgrade: websocket\r\nX-Keep: 1\r\n").await;
    assert!(!has_header(&request, "upgrade"), "{request}");
    assert_eq!(header_lines(&request, "x-keep"), ["1"], "{request}");
}

// Responses: what the client receives.

#[tokio::test]
async fn hop_by_hop_and_nominated_response_headers_do_not_reach_the_client() {
    let response = response_seen(
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive, X-Private\r\nX-Private: 1\r\nKeep-Alive: timeout=5\r\nProxy-Authenticate: Basic\r\nProxy-Connection: keep-alive\r\nTE: trailers\r\nTrailer: X-Sum\r\nX-Kept: 1\r\n\r\nok",
    )
    .await;
    assert_eq!(status(&response), "HTTP/1.1 200 OK", "{response}");
    for name in [
        "x-private",
        "keep-alive",
        "proxy-authenticate",
        "proxy-connection",
        "te",
        "trailer",
    ] {
        assert!(
            !has_header(&response, name),
            "{name} reached the client: {response}"
        );
    }
    assert_eq!(header_lines(&response, "x-kept"), ["1"], "{response}");
    assert!(response.ends_with("\r\n\r\nok"), "{response}");
}

#[tokio::test]
async fn response_connection_options_are_matched_across_headers_case_and_empty_elements() {
    let response = response_seen(
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nconnection: , X-One ,,\r\nCONNECTION:x-TWO\r\nX-One: 1\r\nx-two: 2\r\nX-Two: 3\r\nX-Three: 3\r\n\r\nok",
    )
    .await;
    assert_eq!(status(&response), "HTTP/1.1 200 OK", "{response}");
    assert!(
        !has_header(&response, "x-one") && !has_header(&response, "x-two"),
        "{response}"
    );
    assert_eq!(header_lines(&response, "x-three"), ["3"], "{response}");
}

#[tokio::test]
async fn a_response_nominating_its_framing_becomes_a_bad_gateway() {
    for nominated in ["Content-Length", "Transfer-Encoding"] {
        let reply: &'static [u8] = Box::leak(
            format!("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: {nominated}\r\n\r\nok")
                .into_bytes()
                .into_boxed_slice(),
        );
        let response = response_seen(reply).await;
        assert_eq!(
            status(&response),
            "HTTP/1.1 502 Bad Gateway",
            "{nominated}: {response}"
        );
    }
}

#[tokio::test]
async fn a_keep_alive_response_keeps_the_client_connection_usable() {
    let wire = Wire::start(
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\nKeep-Alive: timeout=5\r\n\r\nok",
    )
    .await;
    let mut stream = TcpStream::connect(wire.proxy).await.expect("connect");
    for _ in 0..2 {
        stream
            .write_all(format!("GET /r HTTP/1.1\r\n{HOST}\r\n").as_bytes())
            .await
            .expect("write");
        let response = read_response(&mut stream).await;
        assert_eq!(status(&response), "HTTP/1.1 200 OK", "{response}");
        assert!(!has_header(&response, "keep-alive"), "{response}");
    }
    wire.finish();
}

// Host and authority (RUSTSEC-2026-0388): the proxy refuses what the origin could read differently.

#[tokio::test]
async fn a_host_that_differs_from_the_request_target_authority_is_refused() {
    for head in [
        "GET http://a.example/ HTTP/1.1\r\nHost: b.example\r\nConnection: close\r\n\r\n",
        "GET / HTTP/1.1\r\nHost: a.example\r\nHost: b.example\r\nConnection: close\r\n\r\n",
        "GET / HTTP/1.1\r\nHost: user@a.example\r\nConnection: close\r\n\r\n",
        "GET http://user@a.example/ HTTP/1.1\r\nHost: user@a.example\r\nConnection: close\r\n\r\n",
    ] {
        let wire = Wire::start(OK).await;
        let response = wire.exchange(head.as_bytes()).await;
        assert_eq!(
            status(&response),
            "HTTP/1.1 400 Bad Request",
            "{head}: {response}"
        );
        assert!(wire.origin.requests().is_empty(), "{head}");
        wire.finish();
    }
}
