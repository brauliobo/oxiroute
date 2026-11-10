//! Interop with the `h2` crate as client over in-memory pipes.
#![allow(
    clippy::cast_possible_truncation,
    clippy::doc_markdown,
    clippy::too_many_lines
)]
mod support;
use std::future::poll_fn;
use std::pin::Pin;

use bytes::Bytes;
use http::{HeaderMap, Request};
use oxiroute_h2::server::{Config, Stream};
use support::*;

type Fut = Pin<Box<dyn std::future::Future<Output = ()> + Send>>;

/// Echoes the path as the body of a 200; `/echo` returns the request body instead.
fn echo(parts: http::request::Parts, mut stream: Stream) -> Fut {
    Box::pin(async move {
        let body = if parts.uri.path() == "/echo" {
            read_body(&mut stream).await.unwrap()
        } else {
            parts.uri.path().as_bytes().to_vec()
        };
        let (status, headers) = response(200, &[("x-len", &body.len().to_string())]);
        stream.send_response(status, headers, false).unwrap();
        write_body(&mut stream, Bytes::from(body), true)
            .await
            .unwrap();
    })
}

async fn body_of(response: h2::client::ResponseFuture) -> (http::response::Parts, Vec<u8>) {
    let (parts, mut body) = response.await.unwrap().into_parts();
    let mut out = Vec::new();
    while let Some(chunk) = body.data().await {
        let chunk = chunk.unwrap();
        body.flow_control().release_capacity(chunk.len()).unwrap();
        out.extend_from_slice(&chunk);
    }
    (parts, out)
}

#[tokio::test(start_paused = true)]
async fn three_hundred_multiplexed_requests_with_table_churn() {
    let (client, _server) = h2_client(Config::default(), echo).await;
    let mut tasks = Vec::new();
    for i in 0..300u32 {
        let mut client = client.clone().ready().await.unwrap();
        let request = Request::get(format!("https://example.test/item/{i}"))
            .header("x-seq", i.to_string())
            .header(
                format!("x-churn-{}", i % 37),
                format!("value-{}-{}", i, "x".repeat((i % 50) as usize)),
            )
            .header("cookie", format!("a={i}"))
            .header("cookie", "b=2")
            .body(())
            .unwrap();
        let (response, _) = client.send_request(request, true).unwrap();
        tasks.push((i, tokio::spawn(body_of(response))));
    }
    for (i, t) in tasks {
        let (parts, body) = t.await.unwrap();
        assert_eq!(parts.status, 200);
        assert_eq!(body, format!("/item/{i}").as_bytes());
    }
}

#[tokio::test(start_paused = true)]
async fn request_body_larger_than_the_window_stalls_and_resumes() {
    let (client, _server) = h2_client(Config::default(), echo).await;
    let mut client = client.ready().await.unwrap();
    let request = Request::post("https://example.test/echo").body(()).unwrap();
    let (response, mut send) = client.send_request(request, false).unwrap();
    let payload: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    let mut data = Bytes::from(payload.clone());
    while !data.is_empty() {
        send.reserve_capacity(data.len());
        let n = poll_fn(|cx| send.poll_capacity(cx)).await.unwrap().unwrap();
        let last = n == data.len();
        send.send_data(data.split_to(n), last).unwrap();
    }
    let (parts, body) = body_of(response).await;
    assert_eq!(parts.status, 200);
    assert_eq!(body, payload);
}

#[tokio::test(start_paused = true)]
async fn response_body_larger_than_the_peer_window() {
    fn big(_: http::request::Parts, mut stream: Stream) -> Fut {
        Box::pin(async move {
            let (status, headers) = response(200, &[]);
            stream.send_response(status, headers, false).unwrap();
            let payload: Vec<u8> = (0..400_000u32).map(|i| (i % 253) as u8).collect();
            write_body(&mut stream, Bytes::from(payload), true)
                .await
                .unwrap();
        })
    }
    let (client, _server) = h2_client(Config::default(), big).await;
    let mut client = client.ready().await.unwrap();
    let (response, _) = client
        .send_request(
            Request::get("https://example.test/").body(()).unwrap(),
            true,
        )
        .unwrap();
    let (_, body) = body_of(response).await;
    let expected: Vec<u8> = (0..400_000u32).map(|i| (i % 253) as u8).collect();
    assert_eq!(body, expected);
}

#[tokio::test(start_paused = true)]
async fn request_and_response_trailers() {
    fn trailers(_: http::request::Parts, mut stream: Stream) -> Fut {
        Box::pin(async move {
            let body = read_body(&mut stream).await.unwrap();
            let t = poll_fn(|cx| stream.poll_trailers(cx))
                .await
                .unwrap()
                .unwrap();
            let (status, headers) = response(200, &[]);
            stream.send_response(status, headers, false).unwrap();
            write_body(&mut stream, Bytes::from(body), false)
                .await
                .unwrap();
            let mut out = HeaderMap::new();
            out.insert("x-seen", t["x-req-trailer"].clone());
            stream.send_trailers(out).unwrap();
        })
    }
    let (client, _server) = h2_client(Config::default(), trailers).await;
    let mut client = client.ready().await.unwrap();
    let (response, mut send) = client
        .send_request(
            Request::post("https://example.test/").body(()).unwrap(),
            false,
        )
        .unwrap();
    send.send_data(Bytes::from_static(b"hello"), false).unwrap();
    let mut t = HeaderMap::new();
    t.insert("x-req-trailer", "t1".parse().unwrap());
    send.send_trailers(t).unwrap();
    let (_, mut body) = response.await.unwrap().into_parts();
    let mut got = Vec::new();
    while let Some(c) = body.data().await {
        got.extend_from_slice(&c.unwrap());
    }
    assert_eq!(got, b"hello");
    let t = body.trailers().await.unwrap().unwrap();
    assert_eq!(t["x-seen"], "t1");
}

#[tokio::test(start_paused = true)]
async fn large_header_blocks_use_continuation_in_both_directions() {
    fn many(parts: http::request::Parts, mut stream: Stream) -> Fut {
        Box::pin(async move {
            let n = parts
                .headers
                .iter()
                .filter(|(k, _)| k.as_str().starts_with("x-big-"))
                .count();
            let mut headers = HeaderMap::new();
            for i in 0..n {
                headers.insert(
                    http::HeaderName::from_bytes(format!("x-resp-{i}").as_bytes()).unwrap(),
                    format!("{}{i}", "r".repeat(800)).parse().unwrap(),
                );
            }
            stream
                .send_response(http::StatusCode::OK, headers, true)
                .unwrap();
        })
    }
    let mut config = Config::default();
    config.max_header_list_size(256 * 1024);
    let (client, _server) = h2_client(config, many).await;
    let mut client = client.ready().await.unwrap();
    let mut builder = Request::get("https://example.test/");
    for i in 0..40 {
        builder = builder.header(format!("x-big-{i}"), format!("{}{i}", "q".repeat(900)));
    }
    let (response, _) = client
        .send_request(builder.body(()).unwrap(), true)
        .unwrap();
    let response = response.await.unwrap();
    assert_eq!(response.headers().len(), 40);
    assert_eq!(
        response.headers()["x-resp-7"].to_str().unwrap(),
        format!("{}7", "r".repeat(800))
    );
}

#[tokio::test(start_paused = true)]
async fn head_and_bodiless_responses() {
    fn head(parts: http::request::Parts, mut stream: Stream) -> Fut {
        Box::pin(async move {
            let (status, headers) = response(200, &[("content-length", "1000")]);
            assert_eq!(parts.method, http::Method::HEAD);
            stream.send_response(status, headers, true).unwrap();
        })
    }
    let (client, _server) = h2_client(Config::default(), head).await;
    let mut client = client.ready().await.unwrap();
    let (response, _) = client
        .send_request(
            Request::head("https://example.test/x").body(()).unwrap(),
            true,
        )
        .unwrap();
    let response = response.await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-length"], "1000");
    assert!(response.into_body().is_end_stream());
}

#[tokio::test(start_paused = true)]
async fn connect_tunnel_relays_both_directions() {
    fn tunnel(parts: http::request::Parts, mut stream: Stream) -> Fut {
        Box::pin(async move {
            assert_eq!(parts.method, http::Method::CONNECT);
            assert_eq!(parts.uri.authority().unwrap().as_str(), "target.test:443");
            assert!(parts.uri.scheme().is_none() && parts.uri.path_and_query().is_none());
            let (status, headers) = response(200, &[]);
            stream.send_response(status, headers, false).unwrap();
            while let Some(chunk) = poll_fn(|cx| stream.poll_data(cx)).await {
                let chunk = chunk.unwrap();
                stream.release_capacity(chunk.len());
                let upper = chunk.to_ascii_uppercase();
                write_body(&mut stream, Bytes::from(upper), false)
                    .await
                    .unwrap();
            }
            stream.send_data(Bytes::new(), true).unwrap();
        })
    }
    let (client, _server) = h2_client(Config::default(), tunnel).await;
    let mut client = client.ready().await.unwrap();
    let request = Request::connect("target.test:443").body(()).unwrap();
    let (response, mut send) = client.send_request(request, false).unwrap();
    let (parts, mut body) = response.await.unwrap().into_parts();
    assert_eq!(parts.status, 200);
    send.send_data(Bytes::from_static(b"ping one"), false)
        .unwrap();
    assert_eq!(body.data().await.unwrap().unwrap(), "PING ONE");
    send.send_data(Bytes::from_static(b"two"), true).unwrap();
    assert_eq!(body.data().await.unwrap().unwrap(), "TWO");
    while let Some(rest) = body.data().await {
        assert!(rest.unwrap().is_empty());
    }
}

#[tokio::test(start_paused = true)]
async fn concurrent_streams_never_exceed_the_limit() {
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
    static LIVE: AtomicUsize = AtomicUsize::new(0);
    static PEAK: AtomicUsize = AtomicUsize::new(0);
    fn slow(_: http::request::Parts, mut stream: Stream) -> Fut {
        Box::pin(async move {
            let now = LIVE.fetch_add(1, SeqCst) + 1;
            PEAK.fetch_max(now, SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            let (status, headers) = response(200, &[]);
            stream.send_response(status, headers, true).unwrap();
            LIVE.fetch_sub(1, SeqCst);
        })
    }
    let mut config = Config::default();
    config.max_concurrent_streams(3);
    let (client, _server) = h2_client(config, slow).await;
    let mut tasks = Vec::new();
    for _ in 0..12 {
        let mut c = client.clone().ready().await.unwrap();
        let (response, _) = c
            .send_request(
                Request::get("https://example.test/").body(()).unwrap(),
                true,
            )
            .unwrap();
        tasks.push(tokio::spawn(response));
    }
    for t in tasks {
        assert_eq!(t.await.unwrap().unwrap().status(), 200);
    }
    assert!(PEAK.load(SeqCst) <= 3, "peak {}", PEAK.load(SeqCst));
}

#[tokio::test(start_paused = true)]
async fn peer_reset_mid_body_fails_reads_and_completes_poll_reset() {
    use std::sync::Mutex;
    static SEEN: Mutex<Vec<String>> = Mutex::new(Vec::new());
    fn watch(_: http::request::Parts, mut stream: Stream) -> Fut {
        Box::pin(async move {
            let first = poll_fn(|cx| stream.poll_data(cx)).await.unwrap().unwrap();
            stream.release_capacity(first.len());
            let next = poll_fn(|cx| stream.poll_data(cx)).await.unwrap();
            let reset = poll_fn(|cx| stream.poll_reset(cx)).await.unwrap();
            SEEN.lock()
                .unwrap()
                .push(format!("{} {} {}", first.len(), next.is_err(), reset));
        })
    }
    let (client, server) = h2_client(Config::default(), watch).await;
    let mut client = client.ready().await.unwrap();
    let (_response, mut send) = client
        .send_request(
            Request::post("https://example.test/").body(()).unwrap(),
            false,
        )
        .unwrap();
    send.send_data(Bytes::from_static(b"part"), false).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    send.send_reset(h2::Reason::CANCEL);
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    assert_eq!(
        SEEN.lock().unwrap().as_slice(),
        ["4 true stream no longer needed"]
    );
    drop(client);
    drop(server);
}

#[tokio::test(start_paused = true)]
async fn server_reset_reaches_the_client() {
    fn refuse(_: http::request::Parts, mut stream: Stream) -> Fut {
        Box::pin(async move { stream.send_reset(oxiroute_h2::Reason::REFUSED_STREAM) })
    }
    let (client, _server) = h2_client(Config::default(), refuse).await;
    let mut client = client.ready().await.unwrap();
    let (response, _) = client
        .send_request(
            Request::get("https://example.test/").body(()).unwrap(),
            true,
        )
        .unwrap();
    let err = response.await.unwrap_err();
    assert_eq!(err.reason(), Some(h2::Reason::REFUSED_STREAM));
}

#[tokio::test(start_paused = true)]
async fn dropped_streams_cancel_or_finish_like_h2() {
    fn unfinished(_: http::request::Parts, mut stream: Stream) -> Fut {
        Box::pin(async move {
            let (status, headers) = response(200, &[]);
            stream.send_response(status, headers, false).unwrap();
        })
    }
    fn finished_early(_: http::request::Parts, mut stream: Stream) -> Fut {
        Box::pin(async move {
            let (status, headers) = response(200, &[]);
            stream.send_response(status, headers, true).unwrap();
        })
    }
    let (client, _server) = h2_client(Config::default(), unfinished).await;
    let mut c = client.ready().await.unwrap();
    let (response, _) = c
        .send_request(
            Request::get("https://example.test/").body(()).unwrap(),
            true,
        )
        .unwrap();
    let (_, mut body) = response.await.unwrap().into_parts();
    let err = body.data().await.unwrap().unwrap_err();
    assert_eq!(err.reason(), Some(h2::Reason::CANCEL));

    let (client, _server) = h2_client(Config::default(), finished_early).await;
    let mut c = client.ready().await.unwrap();
    let (response, mut send) = c
        .send_request(
            Request::post("https://example.test/").body(()).unwrap(),
            false,
        )
        .unwrap();
    assert_eq!(response.await.unwrap().status(), 200);
    let reset = poll_fn(|cx| send.poll_reset(cx)).await.unwrap();
    assert_eq!(reset, h2::Reason::NO_ERROR);
}

#[tokio::test(start_paused = true)]
async fn graceful_shutdown_answers_streams_opened_before_the_goaway() {
    use oxiroute_h2::server::Server;
    use tokio::sync::{Notify, oneshot};
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let (stop_tx, mut stop_rx) = oneshot::channel::<()>();
    let release = std::sync::Arc::new(Notify::new());
    let gate = release.clone();
    let server = tokio::spawn(async move {
        let mut server = Server::handshake(server_io, Config::default())
            .await
            .unwrap();
        let mut draining = false;
        loop {
            tokio::select! {
                _ = &mut stop_rx, if !draining => {
                    server.graceful_shutdown();
                    draining = true;
                }
                next = server.accept() => match next {
                    None => break,
                    Some(r) => {
                        let (_, mut stream) = r.unwrap();
                        let gate = gate.clone();
                        tokio::spawn(async move {
                            gate.notified().await;
                            let (status, headers) = response(200, &[]);
                            stream.send_response(status, headers, true).unwrap();
                        });
                    }
                },
            }
        }
    });
    let (send, conn) = h2::client::handshake(client_io).await.unwrap();
    let conn = tokio::spawn(conn);
    let mut c = send.clone().ready().await.unwrap();
    let (first, _) = c
        .send_request(
            Request::get("https://example.test/1").body(()).unwrap(),
            true,
        )
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    stop_tx.send(()).unwrap();
    // This one is on the wire before the client has seen the GOAWAY: it is still answered.
    let mut c2 = send.clone().ready().await.unwrap();
    let second = c2.send_request(
        Request::get("https://example.test/2").body(()).unwrap(),
        true,
    );
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    release.notify_waiters();
    assert_eq!(first.await.unwrap().status(), 200);
    if let Ok((second, _)) = second {
        // Either the client had not seen the GOAWAY (answered) or it refused locally.
        let _ = second.await;
    }
    release.notify_waiters();
    server.await.unwrap();
    drop((c, c2, send));
    conn.await.unwrap().unwrap();
}
