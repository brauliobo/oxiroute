//! HTTP/3 to a plain HTTP/1.1 origin: every way an origin frames its answer, request bodies, reuse and sockets.
#[allow(clippy::wildcard_imports)]
use super::*;

const CHUNKED: &[u8] =
    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n";
const CHUNKED_WITH_TRAILERS: &[u8] =
    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n0\r\nX-Trailer: one\r\n\r\n";
const CLOSE_DELIMITED: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\nuntil-close";
const AFTER_CONTINUE: &[u8] =
    b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";

fn framing_script(request: &Seen) -> Reply {
    let big = vec![b'b'; 300_000];
    match (request.method.as_str(), request.target.as_str()) {
        ("HEAD", "/length") => {
            Reply::Send(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\n".to_vec())
        }
        (_, "/empty") => Reply::Send(response("204 No Content", &[("X-Empty", "yes")], b"")),
        (_, "/not-modified") => {
            Reply::Send(response("304 Not Modified", &[("ETag", "\"v1\"")], b""))
        }
        (_, "/chunked") => Reply::Send(CHUNKED.to_vec()),
        (_, "/trailers") => Reply::Send(CHUNKED_WITH_TRAILERS.to_vec()),
        (_, "/close") => Reply::SendAndClose(CLOSE_DELIMITED.to_vec()),
        (_, "/continue") => Reply::Send(AFTER_CONTINUE.to_vec()),
        (_, "/big") => Reply::Send(response("200 OK", &[], &big)),
        (_, "/missing") => Reply::Send(response("404 Not Found", &[("X-Origin", "seen")], b"nope")),
        _ => ok(b"hello"),
    }
}

#[tokio::test]
pub(super) async fn h3_reaches_a_plain_h1_origin_with_every_response_framing() {
    let (origin, address) = ScriptedOrigin::tcp(framing_script).await;
    let mut daemon = daemon_for(&[address], HttpProxyPolicy::default()).await;

    let answer = daemon.get("/hello").await;
    assert_eq!((answer.status, answer.text().as_str()), (200, "hello"));
    assert_eq!(answer.header("content-length"), Some("5"));
    assert_eq!(answer.header("content-type"), Some("text/plain"));

    let answer = daemon.send("HEAD", "/length", &[], b"").await;
    assert_eq!(
        (answer.status, answer.header("content-length")),
        (200, Some("5"))
    );
    assert!(answer.body.is_empty());

    let answer = daemon.get("/empty").await;
    assert_eq!(
        (answer.status, answer.header("x-empty")),
        (204, Some("yes"))
    );
    assert!(answer.body.is_empty() && answer.header("content-length").is_none());

    let answer = daemon.get("/not-modified").await;
    assert_eq!(
        (answer.status, answer.header("etag")),
        (304, Some("\"v1\""))
    );
    assert!(answer.body.is_empty());

    for (path, expected) in [
        ("/chunked", "hello world"),
        ("/trailers", "abc"),
        ("/close", "until-close"),
        ("/continue", "ok"),
    ] {
        let answer = daemon.get(path).await;
        assert_eq!(
            (answer.status, answer.text().as_str()),
            (200, expected),
            "{path}"
        );
        // An answer of unknown length is sent as it arrives, so it declares none; a declared one is kept.
        let declared = (path == "/continue").then(|| expected.len().to_string());
        assert_eq!(
            answer.header("content-length"),
            declared.as_deref(),
            "{path}"
        );
        assert!(answer.header("transfer-encoding").is_none(), "{path}");
    }

    let answer = daemon.get("/big").await;
    assert_eq!((answer.status, answer.body.len()), (200, 300_000));
    assert_eq!(answer.header("content-length"), Some("300000"));
    assert!(answer.body.iter().all(|byte| *byte == b'b'));

    let answer = daemon.get("/missing").await;
    assert_eq!(
        (
            answer.status,
            answer.header("x-origin"),
            answer.text().as_str()
        ),
        (404, Some("seen"), "nope")
    );

    assert_eq!(origin.requests().len(), 10);
    daemon.finish().await;
}

#[tokio::test]
pub(super) async fn h3_request_bodies_reach_a_plain_h1_origin_with_their_length() {
    let (origin, address) = ScriptedOrigin::tcp(|request| ok(&request.body)).await;
    let mut daemon = daemon_for(&[address], HttpProxyPolicy::default()).await;

    for body in [&b""[..], b"x", &vec![b'p'; 60 * 1024]] {
        let answer = daemon
            .send(
                "POST",
                "/upload",
                &[("content-type", "application/octet-stream")],
                body,
            )
            .await;
        assert_eq!(answer.status, 200);
        assert_eq!(answer.body.as_ref(), body);
    }
    let requests = origin.requests();
    assert_eq!(
        requests[0].header("content-length"),
        None,
        "an empty body without a length stays without one"
    );
    assert_eq!(requests[1].header("content-length"), Some("1"));
    assert_eq!(requests[2].header("content-length"), Some("61440"));
    assert!(
        requests
            .iter()
            .all(|request| request.header("transfer-encoding").is_none())
    );
    assert_eq!(
        requests[1].header("content-type"),
        Some("application/octet-stream")
    );
    daemon.finish().await;
}

#[tokio::test]
pub(super) async fn h3_requests_reuse_one_connection_to_a_plain_h1_origin() {
    let (origin, address) = ScriptedOrigin::tcp(|_| ok(b"again")).await;
    let mut daemon = daemon_for(&[address], HttpProxyPolicy::default()).await;
    for _ in 0..3 {
        assert_eq!(daemon.get("/reuse").await.status, 200);
    }
    assert_eq!(origin.connections(), 1);
    daemon.finish().await;
}

#[tokio::test]
pub(super) async fn h3_requests_open_a_connection_for_each_request_when_a_pool_never_reuses_them() {
    let (origin, address) = ScriptedOrigin::tcp(|_| ok(b"fresh")).await;
    let listener = reserve_udp_address();
    let mut never = tcp_pool("origin", &[address]);
    never.connection_reuse = oxiroute_config::UpstreamConnectionReuse::Never;
    let config = config(
        listener,
        service(vec![route("/", "origin", HttpProxyPolicy::default())]),
        vec![never],
    );
    let mut daemon = Daemon::start(listener, &config).await;
    for _ in 0..2 {
        assert_eq!(daemon.get("/never").await.status, 200);
    }
    assert_eq!(origin.connections(), 2);
    assert!(
        origin
            .requests()
            .iter()
            .all(|request| request.header("connection") == Some("close"))
    );
    daemon.finish().await;
}

#[tokio::test]
pub(super) async fn h3_requests_go_on_when_the_origin_closes_a_connection_between_them() {
    let (origin, address) = ScriptedOrigin::tcp(|request| {
        let closing = request.target == "/closing";
        let reply = response(
            "200 OK",
            &[("Connection", if closing { "close" } else { "keep-alive" })],
            b"ok",
        );
        if request.target == "/silent" {
            Reply::SendAndClose(reply)
        } else {
            Reply::Send(reply)
        }
    })
    .await;
    let mut daemon = daemon_for(&[address], HttpProxyPolicy::default()).await;
    for path in ["/silent", "/after", "/closing", "/last"] {
        let answer = daemon.get(path).await;
        assert_eq!(
            (answer.status, answer.text().as_str()),
            (200, "ok"),
            "{path}"
        );
        sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(origin.connections(), 3);
    daemon.finish().await;
}

#[tokio::test]
pub(super) async fn h3_reaches_an_origin_on_a_unix_socket() {
    let directory = tempfile::TempDir::new().expect("socket directory");
    let path = directory.path().join("origin.sock");
    let origin = ScriptedOrigin::unix(&path, |request| ok(request.target.as_bytes()));
    let listener = reserve_udp_address();
    let pool = pool(
        "origin",
        vec![oxiroute_config::UpstreamEndpoint::Unix { path }],
    );
    let config = config(
        listener,
        service(vec![route("/", "origin", HttpProxyPolicy::default())]),
        vec![pool],
    );
    let mut daemon = Daemon::start(listener, &config).await;
    let answer = daemon.get("/over/unix?x=1").await;
    assert_eq!(
        (answer.status, answer.text().as_str()),
        (200, "/over/unix?x=1")
    );
    assert_eq!(origin.requests()[0].header("host"), Some(AUTHORITY));
    daemon.finish().await;
}

const SLOW_CHUNKED_HEAD: &[u8] =
    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n";

#[tokio::test]
pub(super) async fn h3_delivers_what_an_unfinished_answer_has_before_the_origin_ends_it() {
    let (origin, address) = ScriptedOrigin::tcp(|_| Reply::Paced {
        pieces: vec![
            (
                Duration::ZERO,
                [SLOW_CHUNKED_HEAD, b"5\r\nfirst\r\n"].concat(),
            ),
            (Duration::from_secs(2), b"6\r\nsecond\r\n0\r\n\r\n".to_vec()),
        ],
        close: false,
    })
    .await;
    let mut daemon = daemon_for(&[address], HttpProxyPolicy::default()).await;
    let mut stream = daemon.open("GET", "/events", &[], b"").await;
    let response = stream.recv_response().await.expect("the head");
    assert_eq!(response.status(), 200);
    assert!(response.headers().get("content-length").is_none());
    let first = timeout(Duration::from_millis(1500), recv_chunk(&mut stream))
        .await
        .expect("the first piece arrives while the origin still holds the rest");
    assert_eq!(first.as_ref(), b"first");
    assert_eq!(recv_body(&mut stream).await.as_ref(), b"second");
    assert_eq!(origin.connections(), 1);
    // The origin connection went back to the pool when the body ended.
    assert_eq!(daemon.get("/events-again").await.status, 200);
    assert_eq!(origin.connections(), 1);
    daemon.finish().await;
}

#[tokio::test]
pub(super) async fn h3_resets_an_answer_whose_origin_fails_after_sending_the_head() {
    let (_origin, address) = ScriptedOrigin::tcp(|_| Reply::Paced {
        pieces: vec![
            (
                Duration::ZERO,
                [SLOW_CHUNKED_HEAD, b"5\r\nhello\r\n"].concat(),
            ),
            (Duration::from_millis(300), Vec::new()),
        ],
        close: true,
    })
    .await;
    let mut daemon = daemon_for(&[address], HttpProxyPolicy::default()).await;
    let mut stream = daemon.open("GET", "/broken", &[], b"").await;
    let response = stream.recv_response().await.expect("the head");
    assert_eq!(response.status(), 200);
    let mut received = Vec::new();
    let ended = loop {
        match stream.recv_data().await {
            Ok(Some(mut chunk)) => {
                received.extend_from_slice(&chunk.copy_to_bytes(chunk.remaining()));
            }
            other => break other.map(|_| ()),
        }
    };
    assert_eq!(received, b"hello");
    assert!(
        ended.is_err(),
        "the response is reset, not finished: {ended:?}"
    );
    daemon.finish().await;
}

#[tokio::test]
pub(super) async fn h3_streams_an_answer_longer_than_the_buffer_limit_whole_and_in_order() {
    let size: usize = 3 * 1024 * 1024;
    let body: Vec<u8> = (0..size)
        .map(|index| u8::try_from(index % 251).unwrap())
        .collect();
    let copy = body.clone();
    let (_origin, address) =
        ScriptedOrigin::tcp(move |_| Reply::Send(response("200 OK", &[], &copy))).await;
    let mut daemon = daemon_for(&[address], HttpProxyPolicy::default()).await;
    let answer = daemon.get("/large").await;
    assert_eq!(answer.status, 200);
    assert_eq!(
        answer.header("content-length"),
        Some(size.to_string().as_str())
    );
    assert_eq!(answer.body.as_ref(), body.as_slice());
    daemon.finish().await;
}
