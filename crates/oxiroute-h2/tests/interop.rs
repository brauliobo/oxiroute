//! Interop with the `h2` crate as client over in-memory pipes.
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
