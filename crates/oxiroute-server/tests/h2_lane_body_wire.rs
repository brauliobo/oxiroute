#![cfg(unix)]
#![allow(dead_code, unused_imports, clippy::duplicate_mod)]
//! Requests with a body on the HTTP/2 lane against the classic path: the same uploads go over TLS to two real
//! `oxiroute` processes, one with the lane (the default) and one with `http_fast_lane` off, in front of origins that
//! record what they receive. What a client or an origin can observe must be identical: the response, the request head
//! and every body byte the origin got, whether the origin connection was reused, and how an upload that ends badly
//! (an early answer, a client reset, a length that does not match) ends. The lane process also says which streams it
//! served, so a case cannot pass because both sides took the classic path.

#[path = "support/config.rs"]
mod config_support;
#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/process.rs"]
mod process_support;
#[path = "support/mod.rs"]
mod wire_support;

use std::{
    future::poll_fn,
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use bytes::Bytes;
use http::{HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode};
use oxiroute_config::{AlpnProtocol, ConfigDraft, HttpVersionPolicy};
use process_support::{ServerProcess, reserve_tcp_address};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
    time::{sleep, timeout},
};
use wire_support::{PROXY_SERVER_NAME, tls_connect};

const STEP: Duration = Duration::from_secs(10);
const SERVED: &str = "served by the HTTP/2 lane";
const LIMIT: usize = 8 * 1024 * 1024;

fn patterned(length: usize) -> Vec<u8> {
    (0..length)
        .map(|index| u8::try_from((index * 7 + index / 251) % 251).expect("below 251"))
        .collect()
}

fn find(bytes: &[u8], needle: &[u8]) -> Option<usize> {
    bytes
        .windows(needle.len())
        .position(|window| window == needle)
}

/// One request the origin received: its head, the body bytes it read, whether the body was complete and whether the
/// origin saw the connection end before that.
#[derive(Clone, Debug, Default, PartialEq)]
struct Seen {
    head: String,
    body: Vec<u8>,
    complete: bool,
    aborted: bool,
}

/// A plain HTTP/1.1 origin that records its requests. The path picks what it does:
/// `/early` answers `413` at once and reads on, `/stream` answers at once and echoes the body as a chunked response
/// while it arrives, `/hold` reads the body and never answers, anything else answers once the body is complete.
struct Origin {
    address: SocketAddr,
    seen: Arc<Mutex<Vec<Seen>>>,
    accepted: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl Origin {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
        let address = listener.local_addr().expect("origin address");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let accepted = Arc::new(AtomicUsize::new(0));
        let (records, count) = (Arc::clone(&seen), Arc::clone(&accepted));
        let task = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.expect("origin accept");
                count.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(serve(stream, Arc::clone(&records)));
            }
        });
        Self {
            address,
            seen,
            accepted,
            task,
        }
    }

    fn requests(&self) -> Vec<Seen> {
        self.seen.lock().expect("records").clone()
    }

    fn body_bytes(&self) -> usize {
        self.requests().iter().map(|seen| seen.body.len()).sum()
    }
}

fn content_length(head: &str) -> usize {
    head.lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().ok())?
        })
        .unwrap_or(0)
}

/// The body a chunked message carries.
fn dechunk(mut raw: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    while let Some(line_end) = find(raw, b"\r\n") {
        let size = usize::from_str_radix(std::str::from_utf8(&raw[..line_end]).unwrap_or("0"), 16)
            .unwrap_or(0);
        if size == 0 || raw.len() < line_end + 2 + size {
            break;
        }
        body.extend_from_slice(&raw[line_end + 2..line_end + 2 + size]);
        raw = raw.get(line_end + 2 + size + 2..).unwrap_or_default();
    }
    body
}

/// What the origin says before it has the whole body: `/early` refuses it, `/stream` starts echoing it.
async fn answer_ahead(stream: &mut TcpStream, path: &str, body: &[u8]) -> std::io::Result<()> {
    if path.starts_with("/early") {
        let reply =
            b"HTTP/1.1 413 Payload Too Large\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        stream.write_all(reply).await?;
    }
    if path.starts_with("/stream") {
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
            .await?;
        if !body.is_empty() {
            echo(stream, body).await?;
        }
    }
    Ok(())
}

async fn serve(mut stream: TcpStream, records: Arc<Mutex<Vec<Seen>>>) {
    let mut bytes = Vec::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let end = loop {
            if let Some(position) = find(&bytes, b"\r\n\r\n") {
                break position + 4;
            }
            match stream.read(&mut buffer).await {
                Ok(0) | Err(_) => return,
                Ok(count) => bytes.extend_from_slice(&buffer[..count]),
            }
        };
        let head = String::from_utf8_lossy(&bytes[..end]).into_owned();
        let path = head.split(' ').nth(1).unwrap_or("/").to_owned();
        let length = content_length(&head);
        let chunked = head
            .to_ascii_lowercase()
            .contains("transfer-encoding: chunked");
        let mut body = bytes.split_off(end);
        bytes.clear();
        let index = {
            let mut records = records.lock().expect("records");
            records.push(Seen {
                head,
                ..Seen::default()
            });
            records.len() - 1
        };
        let note = |body: &[u8], complete: bool, aborted: bool| {
            let mut records = records.lock().expect("records");
            records[index].body = body.to_vec();
            records[index].complete = complete;
            records[index].aborted = aborted;
        };
        if answer_ahead(&mut stream, &path, &body).await.is_err() {
            return;
        }
        let whole = |body: &[u8]| {
            if chunked {
                body.ends_with(b"0\r\n\r\n")
            } else {
                body.len() >= length
            }
        };
        while !whole(&body) || path.starts_with("/early") {
            note(&body, false, false);
            match stream.read(&mut buffer).await {
                Ok(0) | Err(_) => {
                    note(&body, false, true);
                    return;
                }
                Ok(count) => {
                    body.extend_from_slice(&buffer[..count]);
                    if path.starts_with("/stream")
                        && echo(&mut stream, &buffer[..count]).await.is_err()
                    {
                        return;
                    }
                    if path.starts_with("/early") && body.len() >= length {
                        note(&body, false, false);
                    }
                }
            }
        }
        if chunked {
            body = dechunk(&body);
        }
        note(&body, true, false);
        if path.starts_with("/stream") {
            if stream.write_all(b"0\r\n\r\n").await.is_err() {
                return;
            }
            continue;
        }
        if path.starts_with("/hold") {
            let _ = stream.read(&mut buffer).await;
            return;
        }
        let reply = format!("received {} bytes", body.len());
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{reply}",
            reply.len()
        );
        if stream.write_all(response.as_bytes()).await.is_err() {
            return;
        }
    }
}

async fn echo(stream: &mut TcpStream, data: &[u8]) -> std::io::Result<()> {
    stream
        .write_all(&[format!("{:x}\r\n", data.len()).as_bytes(), data, b"\r\n"].concat())
        .await
}

/// One `oxiroute` process and its origin.
struct Side {
    server: ServerProcess,
    origin: Origin,
    address: SocketAddr,
    _config: wire_support::ValidatedProxyConfig,
}

impl Side {
    async fn start(lane: bool) -> Self {
        Self::start_with(lane, |_| {}).await
    }

    async fn start_with(lane: bool, configure: impl Fn(&mut ConfigDraft)) -> Self {
        let origin = Origin::start().await;
        let address = reserve_tcp_address();
        let mut config = wire_support::proxy_config(
            address,
            origin.address,
            vec![AlpnProtocol::H2, AlpnProtocol::Http11],
            None,
            HttpVersionPolicy::default(),
        );
        config.http_services[0].max_request_body_bytes = Some(LIMIT as u64);
        config.http_fast_lane = (!lane).then_some(false);
        config.log_filter = Some("oxiroute::h2lane=debug".into());
        configure(&mut config);
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

    async fn client(&self) -> Client {
        let stream = tls_connect(self.address, PROXY_SERVER_NAME, "ca-a.pem", &[b"h2"])
            .await
            .expect("H2 client connection");
        let (sender, connection) = h2::client::handshake(stream).await.expect("handshake");
        Client {
            sender,
            driver: tokio::spawn(connection),
        }
    }

    /// Stops the process; the number of streams it served by the lane.
    fn served(self) -> usize {
        self.origin.task.abort();
        let output = self.server.shutdown_output();
        output.lines().filter(|line| line.contains(SERVED)).count()
    }
}

struct Client {
    sender: h2::client::SendRequest<Bytes>,
    driver: JoinHandle<Result<(), h2::Error>>,
}

/// What a client sends: the body in pieces with a pause after each, the fields besides the method and the path, and
/// whether the length is announced.
#[derive(Clone)]
struct Upload {
    method: Method,
    path: &'static str,
    fields: Vec<(&'static str, String)>,
    pieces: Vec<Vec<u8>>,
    pause: Duration,
}

impl Upload {
    fn post(path: &'static str, body: Vec<u8>) -> Self {
        Self {
            method: Method::POST,
            path,
            fields: vec![("content-length", body.len().to_string())],
            pieces: vec![body],
            pause: Duration::ZERO,
        }
    }

    fn pieces(path: &'static str, pieces: Vec<Vec<u8>>, pause: Duration) -> Self {
        let total: usize = pieces.iter().map(Vec::len).sum();
        Self {
            fields: vec![("content-length", total.to_string())],
            pieces,
            pause,
            ..Self::post(path, Vec::new())
        }
    }

    fn without_length(mut self) -> Self {
        self.fields.retain(|(name, _)| *name != "content-length");
        self
    }

    fn with(mut self, name: &'static str, value: &str) -> Self {
        self.fields.push((name, value.to_owned()));
        self
    }

    fn declaring(mut self, length: usize) -> Self {
        self.fields.retain(|(name, _)| *name != "content-length");
        self.fields.push(("content-length", length.to_string()));
        self
    }

    fn request(&self) -> Request<()> {
        let mut request = Request::builder()
            .method(self.method.clone())
            .uri(format!("https://{PROXY_SERVER_NAME}{}", self.path));
        for (name, value) in &self.fields {
            request = request.header(*name, value.as_str());
        }
        request.body(()).expect("request")
    }
}

/// What the client saw: status, fields in order (dates and generated ids masked) and body; or how the exchange failed.
type Outcome = Result<(u16, Vec<(String, String)>, Vec<u8>), String>;

fn masked(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(name, value)| {
            let value = value.to_str().unwrap_or("<binary>");
            let value = match name.as_str() {
                "date" => "<date>",
                _ if value.starts_with("rq-") => "rq-<generated>",
                _ => value,
            };
            (name.as_str().to_owned(), value.to_owned())
        })
        .collect()
}

async fn send_pieces(
    send: &mut h2::SendStream<Bytes>,
    pieces: Vec<Vec<u8>>,
    pause: Duration,
    end: bool,
) -> Result<(), h2::Error> {
    let last = pieces.len().saturating_sub(1);
    for (index, piece) in pieces.into_iter().enumerate() {
        let mut data = Bytes::from(piece);
        let finishes = end && index == last;
        if data.is_empty() {
            send.send_data(data, finishes)?;
            continue;
        }
        while !data.is_empty() {
            send.reserve_capacity(data.len());
            let granted = poll_fn(|cx| send.poll_capacity(cx))
                .await
                .ok_or_else(|| h2::Error::from(h2::Reason::CANCEL))??;
            let chunk = data.split_to(granted.min(data.len()));
            send.send_data(chunk, data.is_empty() && finishes)?;
        }
        if !pause.is_zero() {
            sleep(pause).await;
        }
    }
    Ok(())
}

/// The response to `upload`, read while the upload is still going: the first answer wins and what is left of the upload
/// is dropped, as a client that got its answer does.
async fn exchange(client: &mut Client, upload: &Upload) -> Outcome {
    let mut sender = client
        .sender
        .clone()
        .ready()
        .await
        .map_err(|error| format!("ready: {error}"))?;
    let (response, mut send) = sender
        .send_request(upload.request(), false)
        .map_err(|error| format!("send: {error}"))?;
    let pieces = upload.pieces.clone();
    let pause = upload.pause;
    let sending = tokio::spawn(async move {
        send_pieces(&mut send, pieces, pause, true)
            .await
            .map(|()| send)
    });
    let outcome = async {
        let response = response
            .await
            .map_err(|error| format!("response: {error}"))?;
        let (parts, mut body) = response.into_parts();
        let mut bytes = Vec::new();
        while let Some(piece) = body.data().await {
            let piece = piece.map_err(|error| format!("body: {error}"))?;
            let _ = body.flow_control().release_capacity(piece.len());
            bytes.extend_from_slice(&piece);
        }
        Ok((parts.status.as_u16(), masked(&parts.headers), bytes))
    };
    let outcome = timeout(STEP, outcome)
        .await
        .unwrap_or_else(|_| Err("timed out".into()));
    sending.abort();
    outcome
}

fn masked_head(head: &str, port: u16) -> String {
    head.lines()
        .map(|line| match line.split_once(": ") {
            Some((name, value)) if name.eq_ignore_ascii_case("x-request-id") => {
                let value = if value.starts_with("rq-") {
                    "rq-<generated>"
                } else {
                    value
                };
                format!("{name}: {value}")
            }
            _ => line.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("\r\n")
        .replace(&port.to_string(), "<port>")
}

/// What an origin observed, with generated ids masked. `exact` keeps the bodies; a body that the origin stops reading
/// is cut at a different byte on each path, so it is compared by whether it is a prefix of what was sent. A request
/// that the origin saw end before any of its body is not compared: the classic path writes the head before it looks
/// at the client and the lane does not write it when the client has already left.
fn observed(side: &Side, exact: bool) -> Vec<Seen> {
    let port = side.origin.address.port();
    side.origin
        .requests()
        .into_iter()
        .filter(|seen| !(seen.aborted && seen.body.is_empty()))
        .map(|seen| Seen {
            head: masked_head(&seen.head, port),
            body: if exact { seen.body } else { Vec::new() },
            ..seen
        })
        .collect()
}

/// Runs `uploads` one after another on one connection of each side and compares everything observable. The lane side
/// must have served `lane_served` streams.
async fn same(uploads: Vec<Upload>, lane_served: usize) {
    same_with(uploads, lane_served, true, |_| {}).await;
}

async fn same_with(
    uploads: Vec<Upload>,
    lane_served: usize,
    exact: bool,
    configure: impl Fn(&mut ConfigDraft),
) {
    let (classic, lane) = (
        Side::start_with(false, &configure).await,
        Side::start_with(true, &configure).await,
    );
    let (mut c, mut l) = (classic.client().await, lane.client().await);
    for (index, upload) in uploads.iter().enumerate() {
        let from_classic = exchange(&mut c, upload).await;
        let from_lane = exchange(&mut l, upload).await;
        assert_eq!(from_lane, from_classic, "response to upload {index}");
    }
    // The origins may still be reading when the client has its answer.
    sleep(Duration::from_millis(300)).await;
    assert_eq!(
        observed(&lane, exact),
        observed(&classic, exact),
        "what the origin received"
    );
    for side in [&lane, &classic] {
        let sent = patterned(3 << 20);
        for seen in side
            .origin
            .requests()
            .iter()
            .filter(|seen| seen.head.contains("/early"))
        {
            assert_eq!(
                seen.body,
                sent[..seen.body.len()],
                "the part of the upload the origin read"
            );
        }
    }
    assert_eq!(
        lane.origin.accepted.load(Ordering::SeqCst),
        classic.origin.accepted.load(Ordering::SeqCst),
        "origin connections"
    );
    c.driver.abort();
    l.driver.abort();
    assert_eq!(classic.served(), 0, "the classic side took the lane");
    assert_eq!(lane.served(), lane_served, "streams the lane served");
}

#[tokio::test(flavor = "multi_thread")]
async fn bodies_reach_the_origin_as_the_classic_path_delivers_them() {
    let sizes = [
        1,
        5,
        1024,
        16_383,
        16_384,
        16_385,
        65_535,
        65_536,
        200_000,
        1 << 20,
    ];
    let uploads: Vec<Upload> = sizes
        .iter()
        .map(|size| Upload::post("/plain", patterned(*size)))
        .collect();
    let served = uploads.len();
    same(uploads, served).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_body_that_arrives_in_pieces_is_delivered_whole() {
    let pieces = vec![
        patterned(10),
        patterned(40_000),
        Vec::new(),
        patterned(3),
        patterned(70_000),
    ];
    let uploads = vec![
        Upload::pieces("/plain", pieces, Duration::from_millis(20)),
        Upload::post("/plain", patterned(100)),
    ];
    same(uploads, 2).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn every_method_with_a_body_and_the_fields_of_the_request_reach_the_origin_as_sent() {
    let body = patterned(2000);
    let mut uploads = Vec::new();
    for method in [Method::PUT, Method::PATCH, Method::DELETE, Method::POST] {
        uploads.push(Upload {
            method,
            ..Upload::post("/plain?a=1&b=%20", body.clone())
        });
    }
    uploads.push(
        Upload::post("/plain", body.clone())
            .with("content-type", "application/octet-stream")
            .with("x-request-id", "client-id-1")
            .with("cookie", "a=1")
            .with("cookie", "b=2"),
    );
    uploads.push(Upload::post("/plain", body).with("x-request-id", "not valid"));
    let served = uploads.len();
    same(uploads, served).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_body_that_is_announced_empty_is_the_classic_paths_on_both_sides() {
    let mut upload = Upload::post("/plain", Vec::new());
    upload.pieces = vec![Vec::new()];
    same(vec![upload], 0).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_origin_that_answers_before_the_upload_ends_gets_no_more_of_it_and_is_not_reused() {
    let uploads = vec![
        Upload::post("/early", patterned(3 << 20)),
        Upload::post("/plain", patterned(100)),
    ];
    same_with(uploads, 2, false, |_| {}).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_response_flows_back_while_the_upload_is_open() {
    for lane in [false, true] {
        let side = Side::start(lane).await;
        let client = side.client().await;
        let mut sender = client.sender.clone().ready().await.expect("ready");
        let upload = Upload::post("/stream", Vec::new()).declaring(24);
        let (response, mut send) = sender
            .send_request(upload.request(), false)
            .expect("request");
        let mut echoed = Vec::new();
        send.send_data(Bytes::from_static(b"aaaaaaaa"), false)
            .expect("first piece");
        let response = timeout(STEP, response)
            .await
            .expect("head")
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let mut body = response.into_body();
        for round in 1..3_u8 {
            while echoed.len() < 8 * usize::from(round) {
                let piece = timeout(STEP, body.data())
                    .await
                    .expect("the echo of an open upload")
                    .expect("piece")
                    .expect("data");
                echoed.extend_from_slice(&piece);
            }
            send.send_data(Bytes::from(vec![b'a' + round; 8]), round == 2)
                .expect("next piece");
        }
        while let Some(piece) = timeout(STEP, body.data()).await.expect("the end") {
            echoed.extend_from_slice(&piece.expect("data"));
        }
        assert_eq!(echoed, [&[b'a'; 8][..], &[b'b'; 8], &[b'c'; 8]].concat());
        client.driver.abort();
        assert_eq!(side.served(), usize::from(lane));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_slow_uploader_is_forwarded_as_it_arrives_and_not_buffered() {
    for lane in [false, true] {
        let side = Side::start(lane).await;
        let client = side.client().await;
        let mut sender = client.sender.clone().ready().await.expect("ready");
        let upload = Upload::post("/plain", Vec::new()).declaring(30_000);
        let (response, mut send) = sender
            .send_request(upload.request(), false)
            .expect("request");
        for round in 0..3_usize {
            send.send_data(Bytes::from(patterned(10_000)), round == 2)
                .expect("piece");
            if round < 2 {
                let wanted = 10_000 * (round + 1);
                timeout(STEP, async {
                    while side.origin.body_bytes() < wanted {
                        sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .expect("the origin has the pieces that were sent before the next one");
            }
        }
        let response = timeout(STEP, response)
            .await
            .expect("answer")
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        client.driver.abort();
        assert_eq!(side.served(), usize::from(lane));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_resets_the_upload_aborts_the_origin_request_and_the_proxy_serves_on() {
    for lane in [false, true] {
        let side = Side::start(lane).await;
        let client = side.client().await;
        let mut sender = client.sender.clone().ready().await.expect("ready");
        let upload = Upload::post("/hold", Vec::new()).declaring(1_000_000);
        let (_response, mut send) = sender
            .send_request(upload.request(), false)
            .expect("request");
        send.send_data(Bytes::from(patterned(5000)), false)
            .expect("piece");
        timeout(STEP, async {
            while side.origin.body_bytes() < 5000 {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the origin has the piece");
        send.send_reset(h2::Reason::CANCEL);
        timeout(STEP, async {
            while !side.origin.requests().iter().any(|seen| seen.aborted) {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the origin sees the abort");
        let mut client = client;
        let outcome = exchange(&mut client, &Upload::post("/plain", patterned(10))).await;
        assert_eq!(outcome.expect("the next request").0, 200);
        let aborted = side.origin.requests()[0].clone();
        assert!(
            !aborted.complete && aborted.body.len() < 1_000_000,
            "{}",
            aborted.body.len()
        );
        client.driver.abort();
        assert_eq!(side.served(), if lane { 2 } else { 0 });
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_body_that_does_not_match_its_announced_length_ends_as_it_does_on_the_classic_path() {
    let uploads = vec![
        Upload::post("/plain", patterned(5)).declaring(10),
        Upload::post("/plain", patterned(10)).declaring(5),
        Upload::post("/plain", patterned(100)),
    ];
    same(uploads, 3).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn requests_with_a_body_the_lane_does_not_take_are_the_classic_paths_on_both_sides() {
    let uploads = vec![
        Upload::post("/plain", patterned(100)).without_length(),
        Upload::post("/plain", patterned(100)).with("expect", "100-continue"),
        Upload::post("/plain", patterned(100)).with("te", "trailers"),
        Upload::post("/plain", patterned(100)).declaring(LIMIT + 1),
    ];
    same(uploads, 0).await;
}
