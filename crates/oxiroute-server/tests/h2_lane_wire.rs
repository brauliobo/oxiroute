#![cfg(unix)]
#![allow(dead_code, unused_imports, clippy::duplicate_mod)]
//! The HTTP/2 lane against the classic path: the same requests go over TLS to two real `oxiroute` processes, one
//! with the lane (the default) and one with `http_fast_lane` off, in front of identical origins. What a client or
//! an origin can observe must be identical (dates and generated ids masked): the response status, fields in their
//! order and body, and the request head the origin received. The lane process also says which streams it served,
//! so a case cannot pass because both sides took the classic path.

#[path = "support/config.rs"]
mod config_support;
#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/process.rs"]
mod process_support;
#[path = "support/mod.rs"]
mod wire_support;

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use bytes::Bytes;
use http::{HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode};
use oxiroute_config::{
    AlpnProtocol, HttpLiteralHeader, HttpPathSelector, HttpRoute, HttpRouteAction,
    HttpVersionPolicy,
};
use process_support::{ServerProcess, reserve_tcp_address};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpListener,
    task::JoinHandle,
    time::timeout,
};
use wire_support::{H2Client, PROXY_SERVER_NAME, tls_connect};

const STEP: Duration = Duration::from_secs(10);
const SERVED: &str = "served by the HTTP/2 lane";

/// The replies of the origin, chosen by the path of the request.
fn reply(path: &str) -> Vec<u8> {
    let big = "x".repeat(64 * 1024);
    match path {
        "/plain" => b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 12\r\nETag: \"v1\"\r\n\r\nhello origin".to_vec(),
        "/big" => format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{big}", big.len()).into_bytes(),
        "/chunked" => b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nX-A: 1\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n".to_vec(),
        "/nocontent" => b"HTTP/1.1 204 No Content\r\nX-A: 1\r\n\r\n".to_vec(),
        "/notmodified" => b"HTTP/1.1 304 Not Modified\r\nETag: \"v1\"\r\n\r\n".to_vec(),
        "/missing" => b"HTTP/1.1 404 Not Found\r\nContent-Length: 10\r\n\r\nnot found\n".to_vec(),
        "/empty" => b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_vec(),
        "/hop" => b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive, X-Private\r\nX-Private: 1\r\nKeep-Alive: timeout=5\r\nProxy-Authenticate: Basic\r\nTE: trailers\r\nX-Kept: 1\r\n\r\nok".to_vec(),
        "/ids" => b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nX-Request-Id: from-origin\r\n\r\nok".to_vec(),
        "/dates" => b"HTTP/1.1 200 OK\r\nX-Before: 1\r\nDate: Thu, 01 Jan 1970 00:00:00 GMT\r\nX-After: 2\r\nContent-Length: 2\r\n\r\nok".to_vec(),
        "/multi" => b"HTTP/1.1 200 OK\r\nSet-Cookie: a=1\r\nX-Dup: 1\r\nSet-Cookie: b=2\r\nX-Dup: 2\r\nContent-Length: 2\r\n\r\nok".to_vec(),
        "/informational" => b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec(),
        "/closing" => b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 2\r\n\r\nok".to_vec(),
        "/delimited" => b"HTTP/1.0 200 OK\r\nX-A: 1\r\n\r\nuntil close".to_vec(),
        _ => b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec(),
    }
}

fn closes(path: &str) -> bool {
    matches!(path, "/closing" | "/delimited")
}

/// A plain HTTP/1.1 origin that records the head of each request.
struct Origin {
    address: SocketAddr,
    heads: Arc<Mutex<Vec<String>>>,
    accepted: Arc<std::sync::atomic::AtomicUsize>,
    task: JoinHandle<()>,
}

impl Origin {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
        let address = listener.local_addr().expect("origin address");
        let heads = Arc::new(Mutex::new(Vec::new()));
        let accepted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (seen, count) = (Arc::clone(&heads), Arc::clone(&accepted));
        let task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.expect("origin accept");
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let seen = Arc::clone(&seen);
                tokio::spawn(async move {
                    let mut bytes = Vec::new();
                    let mut buffer = [0_u8; 4096];
                    loop {
                        while let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                            let head = String::from_utf8_lossy(&bytes[..end + 4]).into_owned();
                            bytes.drain(..end + 4);
                            let path = head
                                .split(' ')
                                .nth(1)
                                .unwrap_or("/")
                                .split('?')
                                .next()
                                .unwrap_or("/")
                                .to_owned();
                            seen.lock().expect("heads").push(head);
                            if path == "/slow" {
                                continue;
                            }
                            if stream.write_all(&reply(&path)).await.is_err() || closes(&path) {
                                return;
                            }
                        }
                        match stream.read(&mut buffer).await {
                            Ok(0) | Err(_) => return,
                            Ok(count) => bytes.extend_from_slice(&buffer[..count]),
                        }
                    }
                });
            }
        });
        Self {
            address,
            heads,
            accepted,
            task,
        }
    }
}

/// One `oxiroute` process and its origin.
struct Side {
    server: ServerProcess,
    origin: Origin,
    address: SocketAddr,
    _config: wire_support::ValidatedProxyConfig,
}

fn fixed_route() -> HttpRoute {
    HttpRoute {
        host: None,
        path: HttpPathSelector::SegmentPrefix {
            value: "/fixed".into(),
        },
        methods: Vec::new(),
        access_policy: None,
        policy: oxiroute_config::HttpRoutePolicy::default(),
        action: HttpRouteAction::FixedResponse {
            status: 200,
            body: "fixed\n".into(),
            headers: vec![HttpLiteralHeader {
                name: "x-fixed".into(),
                value: "yes".into(),
                always: true,
            }],
        },
    }
}

impl Side {
    async fn start(lane: bool) -> Self {
        let origin = Origin::start().await;
        let address = reserve_tcp_address();
        let mut config = wire_support::proxy_config(
            address,
            origin.address,
            vec![AlpnProtocol::H2, AlpnProtocol::Http11],
            None,
            HttpVersionPolicy::default(),
        );
        config.http_services[0].routes.insert(0, fixed_route());
        config.http_fast_lane = (!lane).then_some(false);
        config.log_filter = Some("oxiroute::h2lane=debug".into());
        let validated = config.validate();
        let mut server = ServerProcess::start(&validated, None);
        server.wait_for_tcp(address).await;
        Self {
            server,
            origin,
            address,
            _config: validated,
        }
    }

    async fn client(&self) -> H2Client {
        let stream = tls_connect(self.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"h2"])
            .await
            .expect("H2 client connection");
        H2Client::from_tls(stream)
            .await
            .expect("H2 client handshake")
    }

    /// Stops the process; the number of streams it served by the lane.
    fn served(self) -> usize {
        self.origin.task.abort();
        let output = self.server.shutdown_output();
        output.lines().filter(|line| line.contains(SERVED)).count()
    }
}

fn mask(value: &str) -> String {
    if value.starts_with("rq-") {
        return "rq-<generated>".into();
    }
    value.to_owned()
}

/// What the client saw: status, fields in order, body.
fn seen(response: &wire_support::H2Response) -> (u16, Vec<(String, String)>, Vec<u8>) {
    let headers = response
        .headers
        .iter()
        .map(|(name, value)| {
            let value = value.to_str().unwrap_or("<binary>");
            let value = if name == "date" {
                "<date>".into()
            } else {
                mask(value)
            };
            (name.as_str().to_owned(), value)
        })
        .collect();
    (response.status.as_u16(), headers, response.body.to_vec())
}

fn masked_head(head: &str) -> String {
    head.lines()
        .map(|line| match line.split_once(": ") {
            Some((name, value)) if name.eq_ignore_ascii_case("x-request-id") => {
                format!("{name}: {}", mask(value))
            }
            _ => line.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("\r\n")
}

type Request3 = (Method, &'static str, Vec<(&'static str, &'static str)>);

async fn run(
    client: &mut H2Client,
    requests: &[Request3],
) -> Vec<(u16, Vec<(String, String)>, Vec<u8>)> {
    let mut out = Vec::new();
    for (method, path, fields) in requests {
        let mut headers = HeaderMap::new();
        for (name, value) in fields {
            headers.append(
                HeaderName::from_static(name),
                HeaderValue::from_static(value),
            );
        }
        let response = timeout(
            STEP,
            client.request_with_headers(method.clone(), path, headers),
        )
        .await
        .unwrap_or_else(|_| panic!("{method} {path} timed out"))
        .unwrap_or_else(|error| panic!("{method} {path}: {error}"));
        out.push(seen(&response));
    }
    out
}

/// Runs `requests` on both sides and compares everything observable; the lane side must have served `lane_served`.
async fn same(requests: Vec<Request3>, lane_served: usize) {
    let (classic, lane) = (Side::start(false).await, Side::start(true).await);
    let (mut c, mut l) = (classic.client().await, lane.client().await);
    let from_classic = run(&mut c, &requests).await;
    let from_lane = run(&mut l, &requests).await;
    for (index, (expected, actual)) in from_classic.iter().zip(&from_lane).enumerate() {
        assert_eq!(actual, expected, "response to {:?}", requests[index]);
    }
    let heads = |side: &Side| -> Vec<String> {
        side.origin
            .heads
            .lock()
            .expect("heads")
            .iter()
            .map(|head| masked_head(head))
            .collect()
    };
    assert_eq!(heads(&lane), heads(&classic), "request heads at the origin");
    assert_eq!(
        lane.origin
            .accepted
            .load(std::sync::atomic::Ordering::SeqCst),
        classic
            .origin
            .accepted
            .load(std::sync::atomic::Ordering::SeqCst),
        "origin connections"
    );
    c.finish().await;
    l.finish().await;
    assert_eq!(classic.served(), 0, "the classic side took the lane");
    assert_eq!(lane.served(), lane_served, "streams the lane served");
}

fn get(path: &'static str) -> Request3 {
    (Method::GET, path, Vec::new())
}

#[tokio::test(flavor = "multi_thread")]
async fn origin_replies_are_relayed_as_the_classic_path_relays_them() {
    let paths = [
        "/plain",
        "/big",
        "/chunked",
        "/nocontent",
        "/notmodified",
        "/missing",
        "/empty",
        "/hop",
        "/ids",
        "/dates",
        "/multi",
        "/informational",
        "/closing",
        "/delimited",
    ];
    let mut requests: Vec<Request3> = paths.iter().map(|path| get(path)).collect();
    requests.extend(paths.iter().map(|path| (Method::HEAD, *path, Vec::new())));
    let served = requests.len();
    same(requests, served).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn requests_reach_the_origin_as_the_classic_path_sends_them() {
    let requests: Vec<Request3> = vec![
        (
            Method::GET,
            "/plain?a=1&b=%20",
            vec![("accept", "*/*"), ("user-agent", "probe")],
        ),
        (
            Method::GET,
            "/plain",
            vec![("cookie", "a=1"), ("accept", "x"), ("cookie", "b=2")],
        ),
        (
            Method::GET,
            "/plain",
            vec![
                ("x-request-id", "client-id-1"),
                ("x-dup", "1"),
                ("x-dup", "2"),
            ],
        ),
        (Method::GET, "/plain", vec![("x-request-id", "not valid")]),
        (
            Method::GET,
            "/plain",
            vec![("x-request-id", "one"), ("x-request-id", "two")],
        ),
        (Method::DELETE, "/plain", vec![("content-length", "0")]),
        (
            Method::OPTIONS,
            "/plain",
            vec![
                ("content-type", "text/plain"),
                ("cache-control", "no-cache"),
            ],
        ),
    ];
    let served = requests.len();
    same(requests, served).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn fixed_responses_are_answered_as_the_classic_path_answers_them() {
    same(
        vec![
            get("/fixed"),
            (Method::HEAD, "/fixed", Vec::new()),
            get("/fixed/more?q=1"),
        ],
        3,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn requests_the_lane_does_not_take_are_the_classic_paths_on_both_sides() {
    let requests: Vec<Request3> = vec![
        (Method::GET, "/plain", vec![("te", "trailers")]),
        (Method::GET, "/plain", vec![("trailer", "x")]),
        (Method::GET, "/plain", vec![("expect", "100-continue")]),
        (
            Method::GET,
            "/plain",
            vec![("proxy-authorization", "Basic eA==")],
        ),
        (Method::GET, "/plain", vec![("x-padded", " value")]),
        (Method::GET, "/.well-known/acme-challenge/x", Vec::new()),
    ];
    same(requests, 0).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_resets_while_the_origin_answers_ends_the_exchange() {
    let lane = Side::start(true).await;
    let stream = tls_connect(lane.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"h2"])
        .await
        .expect("H2 client connection");
    let (mut sender, connection) = h2::client::handshake(stream).await.expect("handshake");
    let driver = tokio::spawn(connection);
    // The origin does not answer `/slow`: it never sends a reply to it.
    let request = Request::builder()
        .method(Method::GET)
        .uri(format!("https://{PROXY_SERVER_NAME}/slow"))
        .body(())
        .unwrap();
    let (_response, mut send) = sender.send_request(request, true).expect("request");
    timeout(STEP, async {
        while lane.origin.heads.lock().expect("heads").is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the origin received the request");
    send.send_reset(h2::Reason::CANCEL);
    // The proxy is not stuck: the next request is answered.
    let ready = sender.ready().await.expect("sender");
    let mut sender = ready;
    let request = Request::builder()
        .method(Method::GET)
        .uri(format!("https://{PROXY_SERVER_NAME}/plain"))
        .body(())
        .unwrap();
    let (response, _) = sender.send_request(request, true).expect("request");
    let response = timeout(STEP, response)
        .await
        .expect("answer")
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    drop(sender);
    driver.abort();
    assert_eq!(lane.served(), 2);
}
