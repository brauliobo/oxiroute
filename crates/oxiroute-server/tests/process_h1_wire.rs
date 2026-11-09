#![allow(dead_code, unused_imports, clippy::duplicate_mod)]
//! HTTP/1.1 framing and header handling of the proxied path as seen on the wire: what the origin
//! receives for a request, what the client receives for an origin response, and which malformed or
//! ambiguous messages are refused. The proxy is the real binary, so every hook runs.

#[path = "support/config.rs"]
mod config_support;
#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/process.rs"]
mod process_support;

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use oxiroute_config::{
    ConfigDraft, DnsResolutionPolicy, DownstreamTimeoutPolicy, HttpPathSelector, HttpProxyPolicy,
    HttpRequestHeaderMutation, HttpRequestHeaderValue, HttpResponseHeaderMutation, HttpRoute,
    HttpRouteAction, HttpService, HttpUpstreamHost, HttpVersionPolicy, Listener, Protocol,
    UpstreamAlgorithm, UpstreamPool, UpstreamServer,
};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    time::timeout,
};

use config_support::{empty_config, socket_bind, socket_endpoint};
use process_support::{ServerProcess, reserve_tcp_address};

const STEP: Duration = Duration::from_secs(5);

type Captured = Arc<Mutex<Vec<Vec<u8>>>>;

/// An origin that answers every request with `reply` and records the request bytes it received.
struct Origin {
    address: SocketAddr,
    requests: Captured,
}

impl Origin {
    async fn start(reply: &'static [u8]) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
        let address = listener.local_addr().expect("origin address");
        let requests = Captured::default();
        let seen = Arc::clone(&requests);
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.expect("origin accept");
                let seen = Arc::clone(&seen);
                tokio::spawn(async move {
                    while let Some(request) = read_request(&mut stream).await {
                        seen.lock().expect("captured requests").push(request);
                        if stream.write_all(reply).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        Self { address, requests }
    }

    fn requests(&self) -> Vec<String> {
        let requests = self.requests.lock().expect("captured requests");
        requests
            .iter()
            .map(|request| String::from_utf8_lossy(request).into_owned())
            .collect()
    }
}

/// The head of a request and the body its framing announces; `None` once the peer closed.
async fn read_request(stream: &mut TcpStream) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    let head_end = loop {
        if let Some(position) = find(&bytes, b"\r\n\r\n") {
            break position + 4;
        }
        let count = stream.read(&mut buffer).await.ok()?;
        if count == 0 {
            return None;
        }
        bytes.extend_from_slice(&buffer[..count]);
    };
    let head = String::from_utf8_lossy(&bytes[..head_end]).to_ascii_lowercase();
    let length = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|value| value.trim().parse::<usize>().ok());
    let chunked = head.contains("transfer-encoding: chunked");
    loop {
        let complete = if chunked {
            bytes[head_end..].ends_with(b"0\r\n\r\n")
        } else {
            bytes.len() >= head_end + length.unwrap_or(0)
        };
        if complete {
            return Some(bytes);
        }
        let count = stream.read(&mut buffer).await.ok()?;
        if count == 0 {
            return Some(bytes);
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn config(listen: SocketAddr, origin: SocketAddr, policy: HttpProxyPolicy) -> ConfigDraft {
    let mut config = empty_config();
    config.listeners = vec![Listener {
        name: "wire".into(),
        bind: socket_bind(listen),
        protocol: Protocol::Http,
        service: Some("wire".into()),
        tls_profile: None,
        proxy_protocol: None,
        max_connections: None,
        downstream_timeouts: DownstreamTimeoutPolicy::default(),
    }];
    config.upstream_pools = vec![UpstreamPool {
        name: "origin".into(),
        servers: vec![UpstreamServer {
            name: "origin".into(),
            endpoint: socket_endpoint(origin),
            max_connections: None,
            dns_resolution: DnsResolutionPolicy::OnConnect,
        }],
        endpoints: Vec::new(),
        algorithm: UpstreamAlgorithm::RoundRobin,
        health_check: None,
        passive_health: None,
        tls: None,
        http_versions: HttpVersionPolicy::default(),
        queue_timeout_ms: None,
        connect_timeout_ms: None,
        server_timeout_ms: None,
        connection_reuse: oxiroute_config::UpstreamConnectionReuse::default(),
        registration: None,
        canary: None,
        resilience: oxiroute_config::PoolResilience::default(),
        queue: None,
    }];
    config.http_services = vec![HttpService {
        drain_deadline_ms: None,
        name: "wire".into(),
        routes: vec![HttpRoute {
            host: None,
            path: HttpPathSelector::SegmentPrefix { value: "/".into() },
            methods: Vec::new(),
            access_policy: None,
            policy: oxiroute_config::HttpRoutePolicy::default(),
            action: HttpRouteAction::Proxy {
                upstream_pool: "origin".into(),
                policy: policy.into(),
            },
        }],
        automatic_response_headers: true,
        upstream_io_timeout_ms: 2_000,
        max_request_body_bytes: Some(64 * 1024),
        gzip: None,
        access_log: None,
    }];
    config
}

struct Wire {
    origin: Origin,
    proxy: SocketAddr,
    server: ServerProcess,
}

impl Wire {
    async fn start(reply: &'static [u8]) -> Self {
        Self::start_with(reply, HttpProxyPolicy::default()).await
    }

    async fn start_with(reply: &'static [u8], policy: HttpProxyPolicy) -> Self {
        let origin = Origin::start(reply).await;
        let proxy = reserve_tcp_address();
        let config = config(proxy, origin.address, policy)
            .validate()
            .expect("valid wire config");
        let mut server = ServerProcess::start(&config, None);
        server.wait_for_tcp(proxy).await;
        Self {
            origin,
            proxy,
            server,
        }
    }

    /// Sends `request` on a new connection and returns everything the proxy sends until it closes
    /// the connection or goes quiet for a moment after a complete response.
    async fn exchange(&self, request: &[u8]) -> String {
        let mut stream = TcpStream::connect(self.proxy).await.expect("connect");
        stream.write_all(request).await.expect("write request");
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 4096];
        timeout(STEP, async {
            loop {
                match timeout(Duration::from_millis(300), stream.read(&mut buffer)).await {
                    Ok(Ok(0) | Err(_)) | Err(_) => break,
                    Ok(Ok(count)) => bytes.extend_from_slice(&buffer[..count]),
                }
            }
        })
        .await
        .expect("response");
        String::from_utf8_lossy(&bytes).into_owned()
    }

    fn finish(self) {
        self.server.shutdown();
    }
}

fn status(response: &str) -> &str {
    response.lines().next().unwrap_or_default()
}

fn header_names(head: &str) -> Vec<String> {
    head.split("\r\n")
        .skip(1)
        .take_while(|line| !line.is_empty())
        .filter_map(|line| line.split_once(':'))
        .map(|(name, _)| name.to_owned())
        .collect()
}

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

fn forwarding_policy() -> HttpProxyPolicy {
    HttpProxyPolicy {
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
                name: "x-forwarded-proto".into(),
                value: HttpRequestHeaderValue::DownstreamScheme,
            },
            HttpRequestHeaderMutation::Set {
                name: "x-real-ip".into(),
                value: HttpRequestHeaderValue::ClientIp,
            },
            HttpRequestHeaderMutation::Remove {
                name: "x-internal".into(),
            },
        ],
        response_headers: vec![
            HttpResponseHeaderMutation::Set {
                name: "x-served-by".into(),
                value: "edge".into(),
                always: true,
            },
            HttpResponseHeaderMutation::Remove {
                name: "x-origin".into(),
            },
        ],
        ..HttpProxyPolicy::default()
    }
}

fn header_lines(head: &str, name: &str) -> Vec<String> {
    head.split("\r\n")
        .skip(1)
        .take_while(|line| !line.is_empty())
        .filter_map(|line| line.split_once(": "))
        .filter(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.to_owned())
        .collect()
}

#[tokio::test]
async fn forwarded_headers_and_the_host_rewrite_reach_the_origin_in_place() {
    let wire = Wire::start_with(OK, forwarding_policy()).await;
    let response = wire
        .exchange(
            b"GET /f HTTP/1.1\r\nHost: example.test\r\nX-Forwarded-For: 198.51.100.4\r\nX-Keep: 1\r\nX-Forwarded-For: 203.0.113.8\r\nX-Internal: secret\r\nConnection: close\r\n\r\n",
        )
        .await;
    assert_eq!(status(&response), "HTTP/1.1 200 OK", "{response}");
    let request = &wire.origin.requests()[0];
    assert_eq!(header_lines(request, "host"), ["origin.example:8080"]);
    assert_eq!(
        header_lines(request, "x-forwarded-for"),
        ["198.51.100.4, 203.0.113.8, 127.0.0.1"],
        "{request}"
    );
    assert_eq!(header_lines(request, "x-forwarded-proto"), ["http"]);
    assert_eq!(header_lines(request, "x-real-ip"), ["127.0.0.1"]);
    assert!(header_lines(request, "x-internal").is_empty(), "{request}");
    assert_eq!(header_lines(request, "x-keep"), ["1"]);
    let names = header_names(request);
    let position = |name: &str| names.iter().position(|n| n.eq_ignore_ascii_case(name));
    assert!(position("host") < position("x-forwarded-for"), "{request}");
    assert!(
        position("x-forwarded-for") < position("x-keep"),
        "{request}"
    );
    wire.finish();
}

#[tokio::test]
async fn response_policy_sets_and_removes_headers_and_the_request_id_is_one_header() {
    let wire = Wire::start_with(
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nX-Origin: yes\r\nX-Request-Id: from-origin\r\n\r\nok",
        forwarding_policy(),
    )
    .await;
    let response = wire
        .exchange(b"GET /f HTTP/1.1\r\nHost: example.test\r\nX-Request-Id: client-1\r\nConnection: close\r\n\r\n")
        .await;
    assert_eq!(
        header_lines(&response, "x-served-by"),
        ["edge"],
        "{response}"
    );
    assert!(header_lines(&response, "x-origin").is_empty(), "{response}");
    assert_eq!(
        header_lines(&response, "x-request-id"),
        ["client-1"],
        "{response}"
    );
    assert_eq!(
        header_lines(&response, "x-upstream-request-id"),
        ["from-origin"],
        "{response}"
    );
    let request = &wire.origin.requests()[0];
    assert_eq!(
        header_lines(request, "x-request-id"),
        ["client-1"],
        "{request}"
    );
    wire.finish();
}

#[tokio::test]
async fn a_request_without_an_id_gets_one_for_the_origin_and_the_same_one_back() {
    let wire = Wire::start(OK).await;
    let response = wire
        .exchange(b"GET /f HTTP/1.1\r\nHost: example.test\r\nConnection: close\r\n\r\n")
        .await;
    let sent = header_lines(&wire.origin.requests()[0], "x-request-id");
    let received = header_lines(&response, "x-request-id");
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(sent[0].starts_with("rq-"), "{sent:?}");
    assert_eq!(sent, received, "{response}");
    wire.finish();
}

#[tokio::test]
async fn keep_alive_requests_on_one_connection_each_carry_their_own_framing() {
    let wire = Wire::start(OK).await;
    let mut stream = TcpStream::connect(wire.proxy).await.expect("connect");
    for body in ["", "abc", ""] {
        let length = if body.is_empty() {
            String::new()
        } else {
            format!("Content-Length: {}\r\n", body.len())
        };
        let method = if body.is_empty() { "GET" } else { "POST" };
        let request = format!("{method} /k HTTP/1.1\r\nHost: example.test\r\n{length}\r\n{body}");
        stream.write_all(request.as_bytes()).await.expect("write");
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 1024];
        timeout(STEP, async {
            while !bytes.ends_with(b"ok") {
                let count = stream.read(&mut buffer).await.expect("read");
                assert!(
                    count > 0,
                    "closed early: {}",
                    String::from_utf8_lossy(&bytes)
                );
                bytes.extend_from_slice(&buffer[..count]);
            }
        })
        .await
        .expect("response");
    }
    let requests = wire.origin.requests();
    assert_eq!(requests.len(), 3);
    assert!(
        requests[1].starts_with("POST /k") && requests[1].ends_with("\r\n\r\nabc"),
        "{requests:?}"
    );
    assert!(
        !requests[2].to_ascii_lowercase().contains("content-length"),
        "{requests:?}"
    );
    wire.finish();
}
