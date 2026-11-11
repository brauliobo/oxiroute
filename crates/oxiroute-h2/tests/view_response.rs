//! `send_response_fields` and `send_response_with_data` against `send_response` + `poll_capacity` + `send_data`:
//! the same logical response must put the same bytes on the wire, and flow control and resets must behave alike.
#![allow(
    clippy::cast_possible_truncation,
    clippy::doc_markdown,
    clippy::too_many_lines
)]
mod support;
use std::future::Future;

use bytes::Bytes;
use http::{HeaderMap, StatusCode};
use oxiroute_h2::server::{Config, Server, Stream};
use support::*;
use tokio::io::{AsyncWriteExt, duplex};

type Frames = Vec<(u8, u8, u32, Vec<u8>)>;

/// Serves every request with `app` (one task per request, streams kept alive by the task) and returns every frame
/// the server wrote after the connection preface exchange.
#[allow(clippy::needless_pass_by_value)]
fn run<F, Fut>(config: Config, steps: Vec<Vec<u8>>, app: F) -> Frames
where
    F: Fn(usize, Stream) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .start_paused(true)
        .build()
        .unwrap();
    rt.block_on(async {
        let (client, server) = duplex(1 << 22);
        tokio::spawn(async move {
            let Ok(mut server) = Server::handshake(server, config).await else {
                return;
            };
            for n in 0.. {
                let Some(Ok((_view, stream))) = server.accept_view().await else {
                    return;
                };
                tokio::spawn(app(n, stream));
            }
        });
        let mut wire = Wire::new(client);
        wire.start().await;
        wire.settle().await;
        for step in &steps {
            wire.io.write_all(step).await.unwrap();
            wire.io.flush().await.unwrap();
            wire.settle().await;
        }
        wire.frames
            .iter()
            .map(|f| (f.kind, f.flags, f.stream, f.payload.clone()))
            .collect()
    })
}

fn gets(ids: &[u32]) -> Vec<Vec<u8>> {
    let mut c = Client::new();
    ids.iter().map(|&id| c.get(id, "/")).collect()
}

fn config() -> Config {
    Config::default()
}

fn kind(frames: &Frames, kind: u8) -> Vec<&(u8, u8, u32, Vec<u8>)> {
    frames.iter().filter(|f| f.0 == kind).collect()
}

/// A response with every kind of field: never-indexed ones, repeated names, an empty value, a long value.
fn logical_response(n: usize) -> (StatusCode, HeaderMap) {
    let long = "z".repeat(300);
    let id = format!("req-{n}");
    let fields = [
        ("content-type", "text/plain"),
        ("set-cookie", "a=b"),
        ("x-request-id", id.as_str()),
        ("set-cookie", "c=d"),
        ("authorization", "secret"),
        ("proxy-authorization", "secret"),
        ("server", "oxiroute"),
        ("x-empty", ""),
        ("x-long", long.as_str()),
        ("x-same", "1"),
        ("x-same", "2"),
        ("cache-control", "no-store"),
    ];
    let (status, headers) = response([200, 404, 204][n % 3], &fields);
    (status, headers)
}

#[allow(clippy::unused_async)]
async fn via_map(n: usize, mut stream: Stream, end: bool) {
    let (status, headers) = logical_response(n);
    stream.send_response(status, headers, end).unwrap();
}

#[allow(clippy::unused_async)]
async fn via_fields(n: usize, mut stream: Stream, end: bool) {
    let (status, headers) = logical_response(n);
    let fields = headers
        .iter()
        .map(|(n, v)| (n.as_str().as_bytes(), v.as_bytes()));
    stream
        .send_response_fields(status.as_u16(), fields, end)
        .unwrap();
}

#[test]
fn fields_put_the_same_bytes_on_the_wire_as_a_header_map() {
    let steps = gets(&[1, 3, 5, 7, 9, 11]);
    for end in [true, false] {
        let map = run(config(), steps.clone(), move |n, s| via_map(n, s, end));
        let fields = run(config(), steps.clone(), move |n, s| via_fields(n, s, end));
        assert_eq!(kind(&map, HEADERS).len(), 6);
        assert_eq!(map, fields, "end {end}");
    }
}

fn body(len: usize) -> Bytes {
    Bytes::from((0..len).map(|i| (i * 7) as u8).collect::<Vec<_>>())
}

async fn classic(n: usize, mut stream: Stream, len: usize) {
    let (status, headers) = logical_response(n);
    stream.send_response(status, headers, len == 0).unwrap();
    if len > 0 {
        write_body(&mut stream, body(len), true).await.unwrap();
    }
}

async fn combined(n: usize, mut stream: Stream, len: usize) {
    let (status, headers) = logical_response(n);
    let fields = headers
        .iter()
        .map(|(n, v)| (n.as_str().as_bytes(), v.as_bytes()));
    let rest = stream
        .send_response_with_data(status.as_u16(), fields, body(len))
        .unwrap();
    if rest > 0 {
        let tail = body(len).slice(len - rest..);
        write_body(&mut stream, tail, true).await.unwrap();
    }
}

#[test]
fn headers_and_body_in_one_call_put_the_same_bytes_on_the_wire() {
    let mut steps = gets(&[1, 3, 5]);
    steps.push(window_update(0, 400_000));
    steps.extend([
        window_update(1, 100_000),
        window_update(3, 100_000),
        window_update(5, 100_000),
    ]);
    for len in [0, 1, 1024, 16_384, 16_385, 20_000, 65_535, 70_000, 150_000] {
        let a = run(config(), steps.clone(), move |n, s| classic(n, s, len));
        let b = run(config(), steps.clone(), move |n, s| combined(n, s, len));
        assert_eq!(a, b, "body of {len} octets");
        let data: usize = kind(&b, DATA).iter().map(|f| f.3.len()).sum();
        assert_eq!(data, 3 * len, "every octet is sent ({len})");
        let ends = kind(&b, DATA)
            .iter()
            .filter(|f| f.1 & END_STREAM != 0)
            .count();
        assert_eq!(ends, if len == 0 { 0 } else { 3 });
    }
}

#[test]
fn an_empty_body_ends_the_response_with_the_headers() {
    let frames = run(config(), gets(&[1]), |n, s| combined(n, s, 0));
    assert!(kind(&frames, DATA).is_empty());
    assert_ne!(kind(&frames, HEADERS)[0].1 & END_STREAM, 0);
}

async fn report(mut stream: Stream, len: usize, tx: tokio::sync::mpsc::UnboundedSender<usize>) {
    let rest = stream.send_response_with_data(200, [], body(len)).unwrap();
    tx.send(rest).unwrap();
    if rest > 0 {
        write_body(&mut stream, body(len).slice(len - rest..), true)
            .await
            .unwrap();
    }
}

fn remainders(config: Config, steps: Vec<Vec<u8>>, lens: Vec<usize>) -> (Vec<usize>, Frames) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let frames = run(config, steps, move |n, s| report(s, lens[n], tx.clone()));
    let mut out = Vec::new();
    while let Ok(r) = rx.try_recv() {
        out.push(r);
    }
    (out, frames)
}

#[test]
fn the_stream_window_limits_what_is_sent_at_once() {
    let mut c = Client::new();
    let settings = settings(&[(4, 100)]);
    let steps = vec![settings, c.get(1, "/"), window_update(1, 500)];
    let (rest, frames) = remainders(config(), steps, vec![300]);
    assert_eq!(rest, vec![200]);
    let data = kind(&frames, DATA);
    assert_eq!(data.len(), 2);
    assert_eq!((data[0].3.len(), data[0].1 & END_STREAM), (100, 0));
    assert_eq!((data[1].3.len(), data[1].1 & END_STREAM), (200, END_STREAM));
    let sent: Vec<u8> = data.iter().flat_map(|f| f.3.clone()).collect();
    assert_eq!(sent, body(300));
}

#[test]
fn a_body_over_the_window_leaves_the_response_open_until_the_rest_is_sent() {
    let steps = gets(&[1]);
    let (rest, frames) = remainders(config(), steps, vec![70_000]);
    assert_eq!(rest, vec![70_000 - 65_535]);
    let data = kind(&frames, DATA);
    assert_eq!(data.iter().map(|f| f.3.len()).sum::<usize>(), 65_535);
    assert!(data.iter().all(|f| f.1 & END_STREAM == 0));
}

#[test]
fn the_connection_window_is_shared_between_responses() {
    let steps = gets(&[1, 3]);
    let (rest, frames) = remainders(config(), steps, vec![40_000, 40_000]);
    assert_eq!(rest, vec![0, 40_000 - (65_535 - 40_000)]);
    let sent: usize = kind(&frames, DATA).iter().map(|f| f.3.len()).sum();
    assert_eq!(sent, 65_535);
}

#[test]
fn octets_granted_to_another_stream_are_not_taken() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let frames = run(config(), gets(&[1, 3]), move |n, mut stream| {
        let tx = tx.clone();
        async move {
            if n == 0 {
                stream
                    .send_response(StatusCode::OK, HeaderMap::new(), false)
                    .unwrap();
                let granted = std::future::poll_fn(|cx| stream.poll_capacity(cx, 60_000))
                    .await
                    .unwrap();
                assert_eq!(granted, 60_000);
                std::future::pending::<()>().await; // keeps the grant reserved
            } else {
                let rest = stream
                    .send_response_with_data(200, [], body(10_000))
                    .unwrap();
                tx.send(rest).unwrap();
            }
        }
    });
    assert_eq!(rx.try_recv().unwrap(), 10_000 - (65_535 - 60_000));
    let sent: usize = kind(&frames, DATA).iter().map(|f| f.3.len()).sum();
    assert_eq!(sent, 65_535 - 60_000);
}

#[test]
fn the_remainder_goes_out_through_poll_capacity_after_a_window_update() {
    let steps = vec![
        gets(&[1]).concat(),
        window_update(1, 10_000),
        window_update(0, 10_000),
    ];
    let (rest, frames) = remainders(config(), steps, vec![70_000]);
    assert_eq!(rest, vec![4_465]);
    let data = kind(&frames, DATA);
    let all: Vec<u8> = data.iter().flat_map(|f| f.3.clone()).collect();
    assert_eq!(all, body(70_000));
    assert_ne!(data.last().unwrap().1 & END_STREAM, 0);
}

#[test]
fn a_reset_stream_refuses_a_response_and_a_second_response_is_refused() {
    let mut c = Client::new();
    let steps = vec![c.post(1, "/"), rst(1, 8)];
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    run(config(), steps, move |_, mut stream| {
        let tx = tx.clone();
        async move {
            std::future::poll_fn(|cx| stream.poll_reset(cx))
                .await
                .unwrap();
            let one = stream.send_response_fields(200, [], true).is_err();
            let two = stream.send_response_with_data(200, [], body(10)).is_err();
            tx.send((one, two)).unwrap();
        }
    });
    assert_eq!(rx.try_recv().unwrap(), (true, true));
}

#[test]
fn a_response_can_be_sent_once() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    run(config(), gets(&[1, 3, 5]), move |n, mut stream| {
        let tx = tx.clone();
        async move {
            let first = match n {
                0 => stream.send_response_fields(200, [], false).map(|()| 0),
                1 => stream.send_response_with_data(200, [], body(5)),
                _ => stream
                    .send_response(StatusCode::OK, HeaderMap::new(), true)
                    .map(|()| 0),
            };
            first.unwrap();
            let again = [
                stream.send_response_fields(200, [], true).is_err(),
                stream.send_response_with_data(200, [], body(5)).is_err(),
                stream
                    .send_response(StatusCode::OK, HeaderMap::new(), true)
                    .is_err(),
            ];
            let data = stream.send_data(body(1), true).is_err();
            tx.send((n, again, data)).unwrap();
        }
    });
    let mut seen = Vec::new();
    while let Ok(r) = rx.try_recv() {
        seen.push(r);
    }
    seen.sort_unstable();
    // after fields with end = false the stream is open (send_data without a grant fails too: nothing was granted)
    assert_eq!(
        seen,
        vec![
            (0, [true; 3], true),
            (1, [true; 3], true),
            (2, [true; 3], true)
        ]
    );
}

#[test]
fn completed_responses_free_their_concurrency_slots() {
    let mut c = Client::new();
    let ids: Vec<u32> = (0..300).map(|i| 2 * i + 1).collect();
    let steps: Vec<Vec<u8>> = ids.iter().map(|&id| c.get(id, "/")).collect();
    let frames = run(config(), steps, |_, mut stream| async move {
        stream.send_response_with_data(200, [], body(100)).unwrap();
    });
    assert_eq!(kind(&frames, HEADERS).len(), 300);
    assert!(kind(&frames, RST_STREAM).is_empty());
}

#[test]
fn the_response_may_end_while_the_request_body_is_still_arriving() {
    let mut c = Client::new();
    let steps = vec![c.post(1, "/"), data(1, b"abc", 0)];
    let frames = run(config(), steps, |_, mut stream| async move {
        stream.send_response_with_data(200, [], body(10)).unwrap();
        drop(stream);
    });
    let resets = kind(&frames, RST_STREAM);
    assert_eq!(
        resets.len(),
        1,
        "the unfinished request is cut short as after send_response"
    );
    assert_eq!(resets[0].3, 0u32.to_be_bytes());
}

#[test]
fn a_large_body_is_framed_by_the_peer_maximum() {
    let steps = vec![settings(&[(5, 20_000)]), gets(&[1]).concat()];
    let frames = run(config(), steps, |n, s| combined(n, s, 50_000));
    let lens: Vec<usize> = kind(&frames, DATA).iter().map(|f| f.3.len()).collect();
    assert_eq!(lens, vec![20_000, 20_000, 10_000]);
}

#[test]
fn the_server_still_answers_through_the_old_api_alongside() {
    let frames = run(config(), gets(&[1, 3]), |n, s| async move {
        if n == 0 {
            classic(n, s, 10).await;
        } else {
            combined(n, s, 10).await;
        }
    });
    assert_eq!(kind(&frames, DATA).len(), 2);
}
