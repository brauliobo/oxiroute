#![allow(dead_code, unused_imports, clippy::duplicate_mod)]
//! HTTP/1.1 framing and header handling of the proxied path as seen on the wire: what the origin
//! receives for a request, what the client receives for an origin response, and which malformed or
//! ambiguous messages are refused. The proxy is the real binary, so every hook runs.

#[path = "support/config.rs"]
mod config_support;
#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/h1_wire.rs"]
mod h1_wire;
#[path = "support/process.rs"]
mod process_support;

use std::time::Duration;

use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpStream,
    time::timeout,
};

use h1_wire::{find, header_names, status, Wire, STEP};

const OK: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nX-Origin: yes\r\n\r\nok";

#[tokio::test]
async fn a_plain_request_reaches_the_origin_with_its_headers_and_the_response_comes_back_unchanged()
{
    let wire = Wire::start(OK).await;
    let response = wire
        .exchange(
            b"GET /a/b?q=1 HTTP/1.1\r\nHost: example.test\r\nX-One: 1\r\nAccept: */*\r\nConnection: close\r\n\r\n",
        )
        .await;
    assert_eq!(status(&response), "HTTP/1.1 200 OK");
    assert!(
        response.contains("X-Origin: yes") || response.contains("x-origin: yes"),
        "{response}"
    );
    assert!(response.ends_with("\r\n\r\nok"), "{response}");
    let requests = wire.origin.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert!(
        request.starts_with("GET /a/b?q=1 HTTP/1.1\r\n"),
        "{request}"
    );
    assert!(request.contains("Host: example.test\r\n"), "{request}");
    assert!(request.contains("x-one: 1\r\n"), "{request}");
    assert!(
        !request.to_ascii_lowercase().contains("connection:"),
        "{request}"
    );
    wire.finish();
}

#[tokio::test]
async fn a_keep_alive_request_without_edits_is_forwarded_verbatim_in_order() {
    let wire = Wire::start(OK).await;
    let response = wire
        .exchange(
            b"GET /x HTTP/1.1\r\nHost: example.test\r\nZ-Last: 1\r\nA-First: 2\r\nX-Dup: a\r\nX-Dup: b\r\n\r\n",
        )
        .await;
    assert_eq!(status(&response), "HTTP/1.1 200 OK");
    let request = &wire.origin.requests()[0];
    let names = header_names(request);
    let position = |name: &str| {
        names
            .iter()
            .position(|candidate| candidate.eq_ignore_ascii_case(name))
            .unwrap_or_else(|| panic!("{name} missing in {request}"))
    };
    assert!(position("host") < position("z-last"));
    assert!(position("z-last") < position("a-first"));
    assert!(position("a-first") < position("x-dup"));
    assert_eq!(
        names
            .iter()
            .filter(|name| name.eq_ignore_ascii_case("x-dup"))
            .count(),
        2
    );
    assert!(
        request.contains("a\r\n") && request.contains("b\r\n"),
        "{request}"
    );
    wire.finish();
}

#[tokio::test]
async fn the_request_body_is_forwarded_with_its_content_length() {
    let wire = Wire::start(OK).await;
    let response = wire
        .exchange(
            b"POST /p HTTP/1.1\r\nHost: example.test\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
        )
        .await;
    assert_eq!(status(&response), "HTTP/1.1 200 OK");
    let request = &wire.origin.requests()[0];
    assert!(
        request
            .to_ascii_lowercase()
            .contains("content-length: 5\r\n"),
        "{request}"
    );
    assert!(request.ends_with("\r\n\r\nhello"), "{request}");
    wire.finish();
}

#[tokio::test]
async fn a_chunked_request_body_is_forwarded_chunked() {
    let wire = Wire::start(OK).await;
    let response = wire
        .exchange(
            b"POST /p HTTP/1.1\r\nHost: example.test\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\nhello\r\n0\r\n\r\n",
        )
        .await;
    assert_eq!(status(&response), "HTTP/1.1 200 OK");
    let request = wire.origin.requests()[0].to_ascii_lowercase();
    assert!(
        request.contains("transfer-encoding: chunked\r\n"),
        "{request}"
    );
    assert!(request.ends_with("5\r\nhello\r\n0\r\n\r\n"), "{request}");
    wire.finish();
}

#[tokio::test]
async fn content_length_with_chunked_encoding_keeps_only_the_encoding_and_closes_the_connection() {
    let wire = Wire::start(OK).await;
    let response = wire
        .exchange(
            b"POST /p HTTP/1.1\r\nHost: example.test\r\nContent-Length: 4\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n",
        )
        .await;
    assert_eq!(status(&response), "HTTP/1.1 200 OK");
    let request = wire.origin.requests()[0].to_ascii_lowercase();
    assert!(
        request.contains("transfer-encoding: chunked\r\n"),
        "{request}"
    );
    assert!(!request.contains("content-length"), "{request}");
    assert!(request.ends_with("5\r\nhello\r\n0\r\n\r\n"), "{request}");
    wire.finish();
}

async fn assert_refused(request: &[u8], expected: &str) {
    let wire = Wire::start(OK).await;
    let response = wire.exchange(request).await;
    assert_eq!(status(&response), expected, "{response}");
    assert!(
        wire.origin.requests().is_empty(),
        "the origin saw {:?}",
        wire.origin.requests()
    );
    wire.finish();
}

#[tokio::test]
async fn conflicting_content_lengths_are_refused_before_the_origin() {
    assert_refused(
        b"POST /p HTTP/1.1\r\nHost: example.test\r\nContent-Length: 3\r\nContent-Length: 5\r\n\r\nhello",
        "HTTP/1.1 400 Bad Request",
    )
    .await;
}

#[tokio::test]
async fn a_comma_list_of_content_lengths_is_refused() {
    assert_refused(
        b"POST /p HTTP/1.1\r\nHost: example.test\r\nContent-Length: 5, 5\r\n\r\nhello",
        "HTTP/1.1 400 Bad Request",
    )
    .await;
}

#[tokio::test]
async fn a_final_encoding_other_than_chunked_is_refused() {
    assert_refused(
        b"POST /p HTTP/1.1\r\nHost: example.test\r\nTransfer-Encoding: chunked, gzip\r\n\r\n0\r\n\r\n",
        "HTTP/1.1 400 Bad Request",
    )
    .await;
}

#[tokio::test]
async fn http_1_0_with_transfer_encoding_is_refused() {
    assert_refused(
        b"POST /p HTTP/1.0\r\nHost: example.test\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n",
        "HTTP/1.1 400 Bad Request",
    )
    .await;
}

#[tokio::test]
async fn an_obsolete_folded_request_header_is_refused() {
    assert_refused(
        b"GET /p HTTP/1.1\r\nHost: example.test\r\nX-Fold: one\r\n two\r\n\r\n",
        "HTTP/1.1 400 Bad Request",
    )
    .await;
}

#[tokio::test]
async fn a_space_before_the_colon_of_a_request_header_is_refused() {
    assert_refused(
        b"GET /p HTTP/1.1\r\nHost: example.test\r\nX-Bad : 1\r\n\r\n",
        "HTTP/1.1 400 Bad Request",
    )
    .await;
}

#[tokio::test]
async fn a_control_byte_in_a_request_header_value_is_refused() {
    assert_refused(
        b"GET /p HTTP/1.1\r\nHost: example.test\r\nX-Bad: a\x01b\r\n\r\n",
        "HTTP/1.1 400 Bad Request",
    )
    .await;
}

#[tokio::test]
async fn a_bare_line_feed_in_the_request_head_reaches_the_origin_as_crlf_lines() {
    let wire = Wire::start(OK).await;
    let response = wire
        .exchange(b"GET /p HTTP/1.1\r\nHost: example.test\nX-Lf: 1\r\n\r\n")
        .await;
    assert_eq!(status(&response), "HTTP/1.1 200 OK", "{response}");
    for request in wire.origin.requests() {
        assert_eq!(
            request.matches('\n').count(),
            request.matches("\r\n").count(),
            "{request:?}"
        );
    }
    wire.finish();
}

#[tokio::test]
async fn a_request_without_host_is_refused() {
    assert_refused(b"GET /p HTTP/1.1\r\n\r\n", "HTTP/1.1 400 Bad Request").await;
}

#[tokio::test]
async fn a_second_host_header_is_refused() {
    assert_refused(
        b"GET /p HTTP/1.1\r\nHost: a.test\r\nHost: b.test\r\n\r\n",
        "HTTP/1.1 400 Bad Request",
    )
    .await;
}

#[tokio::test]
async fn a_pipelined_request_never_merges_into_the_first_one_for_the_origin() {
    let wire = Wire::start(OK).await;
    let response = wire
        .exchange(
            b"GET /one HTTP/1.1\r\nHost: example.test\r\n\r\nGET /two HTTP/1.1\r\nHost: example.test\r\nConnection: close\r\n\r\n",
        )
        .await;
    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
    let requests = wire.origin.requests();
    assert!(requests[0].starts_with("GET /one "), "{requests:?}");
    assert_eq!(requests[0].matches("HTTP/1.1").count(), 1, "{requests:?}");
    wire.finish();
}

#[tokio::test]
async fn a_request_head_split_across_writes_is_parsed() {
    let wire = Wire::start(OK).await;
    let mut stream = TcpStream::connect(wire.proxy).await.expect("connect");
    for part in [
        &b"GET /split HT"[..],
        b"TP/1.1\r\nHo",
        b"st: example.test\r\nX-A: 1\r",
        b"\n\r",
        b"\n",
    ] {
        stream.write_all(part).await.expect("write part");
        stream.flush().await.expect("flush part");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let mut buffer = [0_u8; 512];
    let count = timeout(STEP, stream.read(&mut buffer))
        .await
        .expect("response")
        .expect("read");
    assert!(buffer[..count].starts_with(b"HTTP/1.1 200 OK"));
    assert!(wire.origin.requests()[0].starts_with("GET /split HTTP/1.1\r\n"));
    wire.finish();
}

#[tokio::test]
async fn many_headers_pass_through() {
    let wire = Wire::start(OK).await;
    let mut request =
        String::from("GET /many HTTP/1.1\r\nHost: example.test\r\nConnection: close\r\n");
    request.extend((0..120).map(|index| format!("X-H{index}: v{index}\r\n")));
    request.push_str("\r\n");
    let response = wire.exchange(request.as_bytes()).await;
    assert_eq!(status(&response), "HTTP/1.1 200 OK");
    let seen = &wire.origin.requests()[0];
    assert!(
        seen.contains("x-h0: v0\r\n") && seen.contains("x-h119: v119\r\n"),
        "{seen}"
    );
    wire.finish();
}

#[tokio::test]
async fn more_headers_than_the_parser_holds_are_refused() {
    let mut request = String::from("GET /many HTTP/1.1\r\nHost: example.test\r\n");
    request.extend((0..300).map(|index| format!("X-H{index}: v\r\n")));
    request.push_str("\r\n");
    assert_refused(request.as_bytes(), "HTTP/1.1 400 Bad Request").await;
}

// Responses from the origin.

async fn reply(origin_reply: &'static [u8], request: &[u8]) -> String {
    let wire = Wire::start(origin_reply).await;
    let response = wire.exchange(request).await;
    wire.finish();
    response
}

const GET: &[u8] = b"GET /r HTTP/1.1\r\nHost: example.test\r\nConnection: close\r\n\r\n";

#[tokio::test]
async fn conflicting_response_content_lengths_become_a_bad_gateway() {
    let response = reply(
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Length: 3\r\n\r\nok!",
        GET,
    )
    .await;
    assert_eq!(status(&response), "HTTP/1.1 502 Bad Gateway", "{response}");
}

#[tokio::test]
async fn a_response_with_both_framings_is_forwarded_chunked_without_the_length() {
    let response = reply(
        b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nok\r\n0\r\n\r\n",
        GET,
    )
    .await;
    let lower = response.to_ascii_lowercase();
    assert_eq!(status(&response), "HTTP/1.1 200 OK", "{response}");
    assert!(!lower.contains("content-length"), "{response}");
    assert!(lower.contains("transfer-encoding: chunked"), "{response}");
    assert!(response.ends_with("2\r\nok\r\n0\r\n\r\n"), "{response}");
}

#[tokio::test]
async fn an_obsolete_folded_response_header_is_unfolded() {
    let response = reply(
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nX-Fold: one\r\n two\r\n\r\nok",
        GET,
    )
    .await;
    assert_eq!(status(&response), "HTTP/1.1 200 OK", "{response}");
    assert!(
        response.to_ascii_lowercase().contains("x-fold: one"),
        "{response}"
    );
    assert!(!response.contains("\r\n two"), "{response}");
}

#[tokio::test]
async fn a_malformed_response_status_line_becomes_a_bad_gateway() {
    let response = reply(b"NOTHTTP 200 OK\r\nContent-Length: 2\r\n\r\nok", GET).await;
    assert_eq!(status(&response), "HTTP/1.1 502 Bad Gateway", "{response}");
}

#[tokio::test]
async fn a_control_byte_in_a_response_header_becomes_a_bad_gateway() {
    let response = reply(
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nX-Bad: a\x01b\r\n\r\nok",
        GET,
    )
    .await;
    assert_eq!(status(&response), "HTTP/1.1 502 Bad Gateway", "{response}");
}

#[tokio::test]
async fn a_response_without_framing_is_forwarded_chunked_and_ends_with_the_origin_close() {
    let response = reply(b"HTTP/1.1 200 OK\r\nX-Open: 1\r\n\r\nopen", GET).await;
    let lower = response.to_ascii_lowercase();
    assert_eq!(status(&response), "HTTP/1.1 200 OK", "{response}");
    assert!(lower.contains("transfer-encoding: chunked"), "{response}");
    assert!(response.contains("4\r\nopen\r\n"), "{response}");
}

#[tokio::test]
async fn hop_by_hop_response_headers_are_not_forwarded() {
    let response = reply(
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: x-private\r\nX-Private: 1\r\nKeep-Alive: timeout=5\r\nX-Kept: 1\r\n\r\nok",
        GET,
    )
    .await;
    let lower = response.to_ascii_lowercase();
    assert!(!lower.contains("x-private"), "{response}");
    assert!(!lower.contains("keep-alive"), "{response}");
    assert!(lower.contains("x-kept: 1"), "{response}");
}

#[tokio::test]
async fn repeated_response_headers_keep_their_order() {
    let response = reply(
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nSet-Cookie: a=1\r\nX-Mid: 1\r\nSet-Cookie: b=2\r\n\r\nok",
        GET,
    )
    .await;
    let first = response.find("a=1").expect("first cookie");
    let second = response.find("b=2").expect("second cookie");
    assert!(first < second, "{response}");
}

#[tokio::test]
async fn a_head_response_has_no_body_and_keeps_the_origin_length() {
    let response = reply(
        b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\n\r\n",
        b"HEAD /r HTTP/1.1\r\nHost: example.test\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert_eq!(status(&response), "HTTP/1.1 200 OK", "{response}");
    assert!(
        response.to_ascii_lowercase().contains("content-length: 7"),
        "{response}"
    );
    assert!(response.ends_with("\r\n\r\n"), "{response}");
}

#[tokio::test]
async fn a_no_content_response_is_forwarded_without_body_or_framing() {
    let response = reply(b"HTTP/1.1 204 No Content\r\nX-A: 1\r\n\r\n", GET).await;
    assert_eq!(status(&response), "HTTP/1.1 204 No Content", "{response}");
    assert!(
        !response.to_ascii_lowercase().contains("transfer-encoding"),
        "{response}"
    );
}
