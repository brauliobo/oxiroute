//! The server layer against the `h3` crate as client over loopback QUIC.
#![allow(
    clippy::cast_possible_truncation,
    clippy::similar_names,
    clippy::too_many_lines
)]
mod common;

use bytes::{Buf, Bytes};
use common::{fixture, within};
use http::{HeaderMap, Method, Request, Response, StatusCode};
use oxiroute_h3::server::{Config, RequestStream};

async fn echo(mut stream: RequestStream) {
    let request = stream.read_head().await.unwrap();
    let mut body = Vec::new();
    while let Some(piece) = stream.recv_data().await.unwrap() {
        body.extend_from_slice(&piece);
    }
    let trailers = stream.recv_trailers().await.unwrap();
    let mut response = Response::builder().status(StatusCode::OK);
    response = response.header("x-method", request.method().as_str());
    response = response.header("x-path", request.uri().path_and_query().unwrap().as_str());
    response = response.header("x-authority", request.uri().authority().unwrap().as_str());
    response = response.header("x-version", format!("{:?}", request.version()));
    if let Some(trailers) = trailers {
        response = response.header("x-trailers", trailers.len().to_string());
    }
    let head_only = request.method() == Method::HEAD;
    stream
        .send_full_response(response.body(()).unwrap(), Bytes::from(body), head_only)
        .await
        .unwrap();
}

async fn read_all(
    stream: &mut h3::client::RequestStream<h3_quinn::BidiStream<Bytes>, Bytes>,
) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(mut chunk) = stream.recv_data().await.unwrap() {
        out.extend_from_slice(&chunk.copy_to_bytes(chunk.remaining()));
    }
    out
}

#[tokio::test]
async fn get_returns_head_and_request_fields() {
    let fx = fixture();
    fx.serve(Config::default(), echo);
    let (mut client, _conn) = fx.h3_client().await;
    within(async {
        let mut stream = client
            .send_request(
                Request::get("https://example.com/a/b?q=1")
                    .body(())
                    .unwrap(),
            )
            .await
            .unwrap();
        stream.finish().await.unwrap();
        let response = stream.recv_response().await.unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["x-method"], "GET");
        assert_eq!(response.headers()["x-path"], "/a/b?q=1");
        assert_eq!(response.headers()["x-authority"], "example.com");
        assert_eq!(response.headers()["x-version"], "HTTP/3.0");
        assert!(read_all(&mut stream).await.is_empty());
    })
    .await;
}

#[tokio::test]
async fn post_body_is_echoed_and_trailers_are_seen() {
    let fx = fixture();
    fx.serve(Config::default(), echo);
    let (mut client, _conn) = fx.h3_client().await;
    within(async {
        let mut stream = client
            .send_request(Request::post("https://example.com/up").body(()).unwrap())
            .await
            .unwrap();
        stream
            .send_data(Bytes::from_static(b"hello "))
            .await
            .unwrap();
        stream
            .send_data(Bytes::from_static(b"world"))
            .await
            .unwrap();
        let mut trailers = HeaderMap::new();
        trailers.insert("x-checksum", "abc".parse().unwrap());
        stream.send_trailers(trailers).await.unwrap();
        stream.finish().await.unwrap();
        let response = stream.recv_response().await.unwrap();
        assert_eq!(response.headers()["x-trailers"], "1");
        assert_eq!(read_all(&mut stream).await, b"hello world");
    })
    .await;
}

#[tokio::test]
async fn head_response_has_no_body() {
    let fx = fixture();
    fx.serve(Config::default(), |mut stream: RequestStream| async move {
        let request = stream.read_head().await.unwrap();
        let response = Response::builder()
            .header("content-length", "5")
            .body(())
            .unwrap();
        let head_only = request.method() == Method::HEAD;
        stream
            .send_full_response(response, Bytes::from_static(b"hello"), head_only)
            .await
            .unwrap();
    });
    let (mut client, _conn) = fx.h3_client().await;
    within(async {
        let mut stream = client
            .send_request(Request::head("https://example.com/").body(()).unwrap())
            .await
            .unwrap();
        stream.finish().await.unwrap();
        assert_eq!(
            stream.recv_response().await.unwrap().headers()["content-length"],
            "5"
        );
        assert!(read_all(&mut stream).await.is_empty());
        let mut stream = client
            .send_request(Request::get("https://example.com/").body(()).unwrap())
            .await
            .unwrap();
        stream.finish().await.unwrap();
        stream.recv_response().await.unwrap();
        assert_eq!(read_all(&mut stream).await, b"hello");
    })
    .await;
}

#[tokio::test]
async fn multi_megabyte_bodies_flow_both_ways() {
    let fx = fixture();
    fx.serve(Config::default(), echo);
    let (mut client, _conn) = fx.h3_client().await;
    let body: Vec<u8> = (0..6_000_000u32).map(|i| (i % 251) as u8).collect();
    within(async {
        let mut stream = client
            .send_request(Request::post("https://example.com/big").body(()).unwrap())
            .await
            .unwrap();
        for piece in body.chunks(100_000) {
            stream
                .send_data(Bytes::copy_from_slice(piece))
                .await
                .unwrap();
        }
        stream.finish().await.unwrap();
        stream.recv_response().await.unwrap();
        assert_eq!(read_all(&mut stream).await, body);
    })
    .await;
}

#[tokio::test]
async fn streamed_response_with_trailers() {
    let fx = fixture();
    fx.serve(Config::default(), |mut stream: RequestStream| async move {
        stream.read_head().await.unwrap();
        stream.send_response(Response::new(())).await.unwrap();
        for i in 0..50u8 {
            stream.send_data(Bytes::from(vec![i; 1000])).await.unwrap();
        }
        let mut trailers = HeaderMap::new();
        trailers.insert("x-done", "yes".parse().unwrap());
        stream.send_trailers(trailers).await.unwrap();
        stream.finish().unwrap();
    });
    let (mut client, _conn) = fx.h3_client().await;
    within(async {
        let mut stream = client
            .send_request(Request::get("https://example.com/").body(()).unwrap())
            .await
            .unwrap();
        stream.finish().await.unwrap();
        stream.recv_response().await.unwrap();
        assert_eq!(read_all(&mut stream).await.len(), 50_000);
        assert_eq!(
            stream.recv_trailers().await.unwrap().unwrap()["x-done"],
            "yes"
        );
    })
    .await;
}

#[tokio::test]
async fn many_concurrent_streams_on_several_connections() {
    let fx = fixture();
    fx.serve(Config::default(), echo);
    within(async {
        let mut tasks = Vec::new();
        for _ in 0..4 {
            let (client, conn) = fx.h3_client().await;
            for i in 0..50u32 {
                let mut client = client.clone();
                tasks.push(tokio::spawn(async move {
                    let uri = format!("https://example.com/{i}");
                    let mut stream = client
                        .send_request(Request::post(uri).body(()).unwrap())
                        .await
                        .unwrap();
                    stream
                        .send_data(Bytes::from(vec![b'z'; 3000 + i as usize]))
                        .await
                        .unwrap();
                    stream.finish().await.unwrap();
                    let response = stream.recv_response().await.unwrap();
                    assert_eq!(response.headers()["x-path"], format!("/{i}").as_str());
                    assert_eq!(read_all(&mut stream).await.len(), 3000 + i as usize);
                }));
            }
            std::mem::forget(conn);
        }
        for task in tasks {
            task.await.unwrap();
        }
    })
    .await;
}

#[tokio::test]
async fn response_status_codes_and_fields_decode_in_the_h3_client() {
    let fx = fixture();
    fx.serve(Config::default(), |mut stream: RequestStream| async move {
        let request = stream.read_head().await.unwrap();
        let code: u16 = request.uri().path()[1..].parse().unwrap();
        let mut response = Response::builder().status(code);
        for (name, value) in [
            ("content-type", "text/css"),
            ("server", ""),
            ("x-odd", "v"),
            ("cache-control", "no-store"),
        ] {
            response = response.header(name, value);
        }
        let empty = matches!(code, 204 | 304) || code < 200;
        let body = if empty {
            Bytes::new()
        } else {
            Bytes::from_static(b"ok")
        };
        stream
            .send_full_response(response.body(()).unwrap(), body, false)
            .await
            .unwrap();
    });
    let (mut client, _conn) = fx.h3_client().await;
    within(async {
        for code in [
            200u16, 204, 206, 301, 302, 304, 400, 403, 404, 418, 421, 425, 500, 503, 599,
        ] {
            let mut stream = client
                .send_request(
                    Request::get(format!("https://example.com/{code}"))
                        .body(())
                        .unwrap(),
                )
                .await
                .unwrap();
            stream.finish().await.unwrap();
            let response = stream.recv_response().await.unwrap();
            assert_eq!(response.status().as_u16(), code);
            assert_eq!(response.headers()["content-type"], "text/css");
            assert_eq!(response.headers()["server"], "");
            assert_eq!(response.headers()["cache-control"], "no-store");
            read_all(&mut stream).await;
        }
    })
    .await;
}

#[tokio::test]
async fn prepared_heads_are_sent_for_get_and_head() {
    let fx = fixture();
    let head = oxiroute_h3::server::PreparedHead::new(
        &Response::builder()
            .status(404)
            .header("content-type", "text/plain")
            .header("x-fixed", "1")
            .body(())
            .unwrap(),
    );
    fx.serve(Config::default(), move |mut stream: RequestStream| {
        let head = head.clone();
        async move {
            let request = stream.read_head().await.unwrap();
            let head_only = request.method() == Method::HEAD;
            stream
                .send_prepared(&head, Bytes::from_static(b"missing"), head_only)
                .await
                .unwrap();
        }
    });
    let (mut client, _conn) = fx.h3_client().await;
    within(async {
        for (method, body) in [(Method::GET, &b"missing"[..]), (Method::HEAD, &b""[..])] {
            let mut stream = client
                .send_request(
                    Request::builder()
                        .method(method)
                        .uri("https://example.com/")
                        .body(())
                        .unwrap(),
                )
                .await
                .unwrap();
            stream.finish().await.unwrap();
            let response = stream.recv_response().await.unwrap();
            assert_eq!(response.status(), 404);
            assert_eq!(response.headers()["x-fixed"], "1");
            assert_eq!(read_all(&mut stream).await, body);
        }
    })
    .await;
}
