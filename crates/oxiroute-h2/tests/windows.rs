//! Receive windows: how many `WINDOW_UPDATE`s an upload costs, that the advertised limits are enforced at large
//! sizes, and that what a connection buffers for an application that reads nothing is bounded by its connection
//! window however many streams carry it.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
mod support;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use bytes::Bytes;
use http::request::Parts;
use oxiroute_h2::server::{Config, Stream};
use support::*;
use tokio::io::duplex;

const MIB: u32 = 1 << 20;
const FLOW_CONTROL: u32 = 3;
const FRAME: usize = 16_384;

type Fut = Pin<Box<dyn Future<Output = ()> + Send>>;

fn windows(stream: u32, connection: u32) -> Config {
    let mut config = Config::default();
    config
        .initial_window_size(stream)
        .initial_connection_window_size(connection);
    config
}

/// Reads the whole request body and answers 200.
fn sink(_: Parts, mut stream: Stream) -> Fut {
    Box::pin(async move {
        read_body(&mut stream).await.unwrap();
        let (status, headers) = response(200, &[]);
        stream.send_response(status, headers, true).unwrap();
    })
}

/// Keeps the stream open without reading: the body stays buffered in the connection.
fn hold(_: Parts, stream: Stream) -> Fut {
    Box::pin(async move {
        std::future::pending::<()>().await;
        drop(stream);
    })
}

fn increment(frame: &Raw) -> u32 {
    u32::from_be_bytes(frame.payload[..4].try_into().unwrap())
}

/// The client's view of what the server allows it to send.
struct Credit {
    stream: i64,
    connection: i64,
}

impl Credit {
    fn from_server(frames: &[Raw]) -> Self {
        let mut credit = Self {
            stream: 65_535,
            connection: 65_535,
        };
        for f in frames {
            match f.kind {
                SETTINGS if f.flags == 0 => {
                    for e in f.payload.chunks(6).filter(|e| e[1] == 4) {
                        credit.stream = i64::from(u32::from_be_bytes(e[2..6].try_into().unwrap()));
                    }
                }
                WINDOW_UPDATE if f.stream == 0 => credit.connection += i64::from(increment(f)),
                _ => {}
            }
        }
        credit
    }
}

struct Upload {
    wire: Wire,
    _server: tokio::task::JoinHandle<Result<(), oxiroute_h2::Error>>,
}

async fn connect(config: Config, handler: Handler) -> Upload {
    let (client_io, server_io) = duplex(4 << 20);
    let server = tokio::spawn(serve_with(server_io, config, handler));
    let mut wire = Wire::new(client_io);
    wire.start().await;
    Upload {
        wire,
        _server: server,
    }
}

impl Upload {
    /// Sends `total` octets in 16 KiB frames on `stream`, waiting for credit as a real client does, then reads to
    /// the end of the response. Returns the `WINDOW_UPDATE`s the server sent after the handshake (every stream).
    async fn upload(&mut self, stream: u32, total: usize) -> Vec<Raw> {
        let first = self.wire.settle().await;
        let handshake = self.wire.frames.len();
        let mut credit = Credit::from_server(&first);
        let mut client = Client::new();
        self.wire.send(&client.post(stream, "/")).await;
        let (mut sent, chunk) = (0, vec![7; FRAME]);
        while sent < total {
            let n = FRAME.min(total - sent);
            while credit.stream < n as i64 || credit.connection < n as i64 {
                let f = tokio::time::timeout(Duration::from_secs(10), self.wire.read_frame())
                    .await
                    .expect("the server returns credit")
                    .expect("connection open");
                if f.kind == WINDOW_UPDATE {
                    let inc = i64::from(increment(&f));
                    if f.stream == 0 {
                        credit.connection += inc;
                    } else {
                        credit.stream += inc;
                    }
                }
            }
            sent += n;
            let end = u8::from(sent == total);
            self.wire.send(&data(stream, &chunk[..n], end)).await;
            credit.stream -= n as i64;
            credit.connection -= n as i64;
        }
        self.wire
            .read_until(|f| f.kind == HEADERS && f.stream == stream)
            .await
            .expect("response");
        self.wire.frames[handshake..]
            .iter()
            .filter(|f| f.kind == WINDOW_UPDATE)
            .cloned()
            .collect()
    }
}

async fn updates_per_mib(config: Config, mib: usize) -> f64 {
    let mut upload = connect(config, sink).await;
    upload.upload(1, mib << 20).await.len() as f64 / mib as f64
}

#[tokio::test(start_paused = true)]
async fn an_upload_costs_sixty_four_window_updates_per_mib_at_the_protocol_default() {
    let per_mib = updates_per_mib(Config::default(), 4).await;
    assert!((60.0..=66.0).contains(&per_mib), "{per_mib}");
}

#[tokio::test(start_paused = true)]
async fn an_upload_costs_about_two_window_updates_per_mib_at_one_and_eight_mib() {
    let per_mib = updates_per_mib(windows(MIB, 8 * MIB), 16).await;
    assert!(per_mib <= 2.5, "{per_mib}");
}

#[tokio::test(start_paused = true)]
async fn the_advertised_windows_are_one_credit_the_client_can_use_whole() {
    let mut upload = connect(windows(MIB, 8 * MIB), hold).await;
    let credit = Credit::from_server(&upload.wire.settle().await);
    assert_eq!(
        (credit.stream, credit.connection),
        (i64::from(MIB), i64::from(8 * MIB))
    );
}

/// Sends `bytes` on `stream` in 16 KiB frames without waiting for anything (the caller knows the credit).
async fn burst(wire: &mut Wire, stream: u32, bytes: usize) {
    let chunk = vec![1; FRAME];
    let mut left = bytes;
    while left > 0 {
        let n = FRAME.min(left);
        wire.send(&data(stream, &chunk[..n], 0)).await;
        left -= n;
    }
}

#[tokio::test(start_paused = true)]
async fn one_octet_beyond_a_large_stream_window_resets_the_stream() {
    let mut upload = connect(windows(MIB, 8 * MIB), hold).await;
    let mut client = Client::new();
    upload.wire.send(&client.post(1, "/")).await;
    upload.wire.settle().await;
    burst(&mut upload.wire, 1, MIB as usize).await;
    assert!(
        upload
            .wire
            .settle()
            .await
            .iter()
            .all(|f| f.kind != 3 && f.kind != GOAWAY)
    );
    upload.wire.send(&data(1, b"x", 0)).await;
    let frames = upload.wire.settle().await;
    let reset = frames.iter().find(|f| f.kind == 3).expect("RST_STREAM");
    assert_eq!((reset.stream, rst_code(reset)), (1, FLOW_CONTROL));
}

#[tokio::test(start_paused = true)]
async fn one_octet_beyond_a_large_connection_window_closes_the_connection() {
    let mut upload = connect(windows(MIB, 4 * MIB), hold).await;
    let mut client = Client::new();
    for stream in [1, 3, 5, 7, 9] {
        upload.wire.send(&client.post(stream, "/")).await;
    }
    upload.wire.settle().await;
    for stream in [1, 3, 5, 7] {
        burst(&mut upload.wire, stream, MIB as usize).await;
    }
    assert!(upload.wire.settle().await.iter().all(|f| f.kind != GOAWAY));
    upload.wire.send(&data(9, b"x", 0)).await;
    let frames = upload.wire.settle().await;
    let goaway = frames.iter().find(|f| f.kind == GOAWAY).expect("GOAWAY");
    assert_eq!(goaway_code(goaway), FLOW_CONTROL);
}

/// Opens `streams` streams on a connection whose application reads nothing and sends on all of them until the
/// server refuses; returns the octets it accepted.
async fn accepted_before_refusal(config: Config, streams: u32) -> usize {
    let mut upload = connect(config, hold).await;
    let mut client = Client::new();
    let ids: Vec<u32> = (0..streams).map(|i| 2 * i + 1).collect();
    for id in &ids {
        upload.wire.send(&client.post(*id, "/")).await;
    }
    upload.wire.settle().await;
    let credit = Credit::from_server(&upload.wire.frames);
    let mut accepted = 0;
    let mut stream_credit = vec![credit.stream; ids.len()];
    'fill: loop {
        let mut progress = false;
        for (i, id) in ids.iter().enumerate() {
            if stream_credit[i] >= FRAME as i64 {
                if accepted + FRAME > credit.connection as usize {
                    break 'fill;
                }
                upload.wire.send(&data(*id, &[1; FRAME], 0)).await;
                stream_credit[i] -= FRAME as i64;
                accepted += FRAME;
                progress = true;
            }
        }
        if !progress {
            break;
        }
    }
    let healthy = upload.wire.settle().await.iter().all(|f| f.kind != GOAWAY);
    assert!(
        healthy,
        "everything within the advertised credit is accepted"
    );
    let probe = ids
        .iter()
        .zip(&stream_credit)
        .find(|(_, c)| **c > 0)
        .map_or(ids[0], |(id, _)| *id);
    upload.wire.send(&data(probe, &[1; FRAME], 0)).await;
    let refused = upload.wire.settle().await;
    assert!(
        refused.iter().any(|f| f.kind == GOAWAY || f.kind == 3),
        "the next frame is refused"
    );
    accepted
}

#[tokio::test(start_paused = true)]
async fn a_stalled_application_holds_at_most_the_connection_window_however_many_streams() {
    for streams in [1, 8, 100] {
        let held = accepted_before_refusal(windows(MIB, 8 * MIB), streams).await;
        assert!(held <= (8 * MIB) as usize, "{streams} streams held {held}");
        assert!(
            held >= (MIB as usize).min((8 * MIB) as usize) - FRAME,
            "{streams}: {held}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn at_the_largest_settings_one_hundred_streams_hold_the_connection_window_and_no_more() {
    let (stream, connection) = (16 * MIB, 64 * MIB);
    let held = accepted_before_refusal(windows(stream, connection), 100).await;
    assert!(held <= connection as usize, "{held}");
    assert!(held + FRAME > connection as usize, "{held}");
}

#[tokio::test(start_paused = true)]
async fn the_h2_client_uploads_through_large_windows() {
    fn echo_len(_: Parts, mut stream: Stream) -> Fut {
        Box::pin(async move {
            let body = read_body(&mut stream).await.unwrap();
            let (status, headers) = response(200, &[("x-len", &body.len().to_string())]);
            stream.send_response(status, headers, true).unwrap();
        })
    }
    let (client, _server) = h2_client(windows(MIB, 8 * MIB), echo_len).await;
    let mut client = client.ready().await.unwrap();
    let request = http::Request::post("https://example.test/")
        .body(())
        .unwrap();
    let (response, mut send) = client.send_request(request, false).unwrap();
    let mut data = Bytes::from(vec![3; 5 << 20]);
    while !data.is_empty() {
        send.reserve_capacity(data.len());
        let n = std::future::poll_fn(|cx| send.poll_capacity(cx))
            .await
            .unwrap()
            .unwrap();
        let last = n == data.len();
        send.send_data(data.split_to(n), last).unwrap();
    }
    let response = response.await.unwrap();
    assert_eq!(response.headers()["x-len"], (5 << 20).to_string().as_str());
}
