//! Protocol violations and abuse limits, driven by a raw quinn client.
#![allow(
    clippy::cast_possible_truncation,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::duration_suboptimal_units
)]
mod common;

use std::time::Duration;

use bytes::Bytes;
use common::raw::{Raw, frame_bytes, read_frames, request_bytes, status_of};
use common::{Fixture, fixture, within};
use http::Response;
use oxiroute_h3::frame;
use oxiroute_h3::server::{Config, RequestStream};

async fn ok(mut stream: RequestStream) {
    if stream.read_head().await.is_ok() {
        let _ = stream.recv_trailers().await;
        let _ = stream
            .send_full_response(Response::new(()), Bytes::from_static(b"ok"), false)
            .await;
    }
}

async fn setup(config: Config) -> (Fixture, Raw) {
    let fx = fixture();
    fx.serve(config, ok);
    let conn = fx
        .client
        .connect(fx.addr, "localhost")
        .unwrap()
        .await
        .unwrap();
    let raw = Raw::new(conn).await;
    (fx, raw)
}

async fn expect_close(raw: &Raw, code: u64) {
    assert_eq!(
        within(raw.closed_with()).await,
        code,
        "close code {code:#x}"
    );
}

async fn answered(raw: &Raw) {
    let (mut send, mut recv) = raw.request(&request_bytes(&[])).await;
    send.finish().unwrap();
    let frames = within(read_frames(&mut recv)).await.unwrap();
    assert_eq!(status_of(&frames[0].1), 200);
    assert_eq!(frames[1], (frame::DATA, b"ok".to_vec()));
}

#[tokio::test]
async fn data_frame_first_is_frame_unexpected() {
    let (_fx, raw) = setup(Config::default()).await;
    let _stream = raw.request(&frame_bytes(frame::DATA, b"")).await;
    expect_close(&raw, 0x105).await;
}

#[tokio::test]
async fn frames_that_may_not_open_a_request_are_frame_unexpected() {
    for kind in [
        frame::SETTINGS,
        frame::GOAWAY,
        frame::PUSH_PROMISE,
        0x2,
        0x8,
    ] {
        let (_fx, raw) = setup(Config::default()).await;
        let _stream = raw.request(&frame_bytes(kind, &[0])).await;
        expect_close(&raw, 0x105).await;
    }
}

#[tokio::test]
async fn a_second_headers_frame_after_trailers_is_frame_unexpected() {
    let (_fx, raw) = setup(Config::default()).await;
    let mut bytes = request_bytes(&[]);
    bytes.extend(frame_bytes(frame::HEADERS, &[0, 0]));
    bytes.extend(frame_bytes(frame::HEADERS, &[0, 0]));
    let _stream = raw.request(&bytes).await;
    expect_close(&raw, 0x105).await;
}

#[tokio::test]
async fn truncated_frame_at_the_end_of_the_stream_is_frame_error() {
    let (_fx, raw) = setup(Config::default()).await;
    let mut bytes = request_bytes(&[]);
    bytes.extend_from_slice(&[frame::DATA as u8, 10, 1, 2]);
    let (mut send, _recv) = raw.request(&bytes).await;
    send.finish().unwrap();
    expect_close(&raw, 0x106).await;
}

#[tokio::test]
async fn unknown_and_grease_frames_on_a_request_stream_are_skipped() {
    let (_fx, raw) = setup(Config::default()).await;
    let mut bytes = frame_bytes(0x1f * 5 + 0x21, &[7; 5000]);
    bytes.extend(request_bytes(&[]));
    bytes.extend(frame_bytes(0x40, b"later"));
    let (mut send, mut recv) = raw.request(&bytes).await;
    send.finish().unwrap();
    let frames = within(read_frames(&mut recv)).await.unwrap();
    assert_eq!(status_of(&frames[0].1), 200);
}

#[tokio::test]
async fn a_huge_unknown_frame_is_skipped_without_buffering() {
    let (_fx, raw) = setup(Config::default()).await;
    let mut header = Vec::new();
    frame::write_header(&mut header, 0x1f * 5 + 0x21, 3_000_000);
    let (mut send, mut recv) = raw.request(&header).await;
    for _ in 0..30 {
        send.write_all(&vec![0u8; 100_000]).await.unwrap();
    }
    send.write_all(&request_bytes(&[])).await.unwrap();
    send.finish().unwrap();
    let frames = within(read_frames(&mut recv)).await.unwrap();
    assert_eq!(status_of(&frames[0].1), 200);
}

#[tokio::test]
async fn oversized_field_section_is_answered_with_431_by_the_application() {
    let fx = fixture();
    fx.serve(Config::default(), |mut stream: RequestStream| async move {
        match stream.read_head().await {
            Err(oxiroute_h3::HeadError::FieldSectionTooLarge { .. }) => {
                let response = Response::builder().status(431).body(()).unwrap();
                stream
                    .send_full_response(response, Bytes::from_static(b"too large"), false)
                    .await
                    .unwrap();
            }
            other => panic!("unexpected {other:?}"),
        }
    });
    let conn = fx
        .client
        .connect(fx.addr, "localhost")
        .unwrap()
        .await
        .unwrap();
    let raw = Raw::new(conn).await;
    let big = "x".repeat(20_000);
    let (mut send, mut recv) = raw.request(&request_bytes(&[("x-padding", &big)])).await;
    send.finish().unwrap();
    let frames = within(read_frames(&mut recv)).await.unwrap();
    assert_eq!(status_of(&frames[0].1), 431);
    assert!(raw.conn.close_reason().is_none());
}

#[tokio::test]
async fn headers_frame_over_the_hard_bound_resets_the_stream() {
    let config = Config {
        max_field_section_size: 1024,
        ..Config::default()
    };
    let (_fx, raw) = setup(config).await;
    let (mut send, mut recv) = raw
        .request(&request_bytes(&[("x-a", &"y".repeat(5000))]))
        .await;
    let _ = send.finish();
    let error = within(read_frames(&mut recv)).await.unwrap_err();
    assert!(
        matches!(error, quinn::ReadError::Reset(code) if u64::from(code) == 0x107),
        "{error:?}"
    );
    assert!(raw.conn.close_reason().is_none());
    answered(&raw).await;
}

#[tokio::test]
async fn malformed_requests_reset_the_stream_with_message_error() {
    let cases: [Vec<(&str, &str)>; 4] = [
        vec![("X-Upper", "1")],
        vec![("connection", "close")],
        vec![("te", "gzip")],
        vec![("transfer-encoding", "chunked")],
    ];
    let (_fx, raw) = setup(Config::default()).await;
    for extra in cases {
        let (mut send, mut recv) = raw.request(&request_bytes(&extra)).await;
        let _ = send.finish();
        let error = within(read_frames(&mut recv)).await.unwrap_err();
        assert!(
            matches!(error, quinn::ReadError::Reset(code) if u64::from(code) == 0x10e),
            "{extra:?}: {error:?}"
        );
    }
    answered(&raw).await;
}

#[tokio::test]
async fn te_trailers_is_accepted() {
    let (_fx, raw) = setup(Config::default()).await;
    let (mut send, mut recv) = raw.request(&request_bytes(&[("te", "trailers")])).await;
    send.finish().unwrap();
    assert_eq!(
        status_of(&within(read_frames(&mut recv)).await.unwrap()[0].1),
        200
    );
}

#[tokio::test]
async fn pseudo_header_rules_are_stream_errors() {
    // no :path, no :method, pseudo after regular, :status in a request, repeated :method
    let blocks: [Vec<u8>; 4] = [
        vec![0, 0, 0xd1, 0xd7, 0x50, 1, b'a'],
        vec![0, 0, 0xd7, 0x50, 1, b'a', 0xc1],
        vec![
            0, 0, 0xd1, 0xd7, 0x50, 1, b'a', 0x23, b'x', b'-', b'a', 1, b'v', 0xc1,
        ],
        vec![0, 0, 0xd1, 0xd7, 0x50, 1, b'a', 0xc1, 0xd1],
    ];
    let (_fx, raw) = setup(Config::default()).await;
    for block in blocks {
        let (mut send, mut recv) = raw.request(&frame_bytes(frame::HEADERS, &block)).await;
        let _ = send.finish();
        let error = within(read_frames(&mut recv)).await.unwrap_err();
        assert!(
            matches!(error, quinn::ReadError::Reset(code) if u64::from(code) == 0x10e),
            "{block:02x?}: {error:?}"
        );
    }
}

#[tokio::test]
async fn dynamic_table_references_close_with_decompression_failed() {
    for block in [
        vec![0x01, 0x00, 0x80],       // Required Insert Count 1
        vec![0x00, 0x00, 0x80],       // indexed, dynamic
        vec![0x00, 0x00, 0x10],       // post-base indexed
        vec![0x00, 0x00, 0x40, 0x00], // dynamic name reference
        vec![0x00, 0x80, 0xc1],       // negative base
        vec![0x00, 0x00, 0xff, 0x24], // static index 99
    ] {
        let (_fx, raw) = setup(Config::default()).await;
        let _stream = raw.request(&frame_bytes(frame::HEADERS, &block)).await;
        expect_close(&raw, 0x200).await;
    }
}

#[tokio::test]
async fn invalid_huffman_closes_with_decompression_failed() {
    let (_fx, raw) = setup(Config::default()).await;
    let block = [&[0u8, 0, 0x50, 0x84][..], &[0xff; 4]].concat();
    let _stream = raw.request(&frame_bytes(frame::HEADERS, &block)).await;
    expect_close(&raw, 0x200).await;
}

fn settings_frame() -> Vec<u8> {
    let mut out = Vec::new();
    oxiroute_h3::frame::settings::encode(&mut out, 100);
    out
}

#[tokio::test]
async fn first_control_frame_other_than_settings_is_missing_settings() {
    let fx = fixture();
    fx.serve(Config::default(), ok);
    let conn = fx
        .client
        .connect(fx.addr, "localhost")
        .unwrap()
        .await
        .unwrap();
    let mut raw = Raw::without_settings(conn).await;
    raw.control.write_all(&[0x00]).await.unwrap();
    raw.control
        .write_all(&frame_bytes(frame::GOAWAY, &[0]))
        .await
        .unwrap();
    expect_close(&raw, 0x10a).await;
}

#[tokio::test]
async fn control_frames_that_are_not_allowed_are_frame_unexpected() {
    for kind in [frame::SETTINGS, frame::DATA, frame::HEADERS, 0x9] {
        let (_fx, mut raw) = setup(Config::default()).await;
        raw.control
            .write_all(&frame_bytes(kind, &settings_frame()[2..]))
            .await
            .unwrap();
        expect_close(&raw, 0x105).await;
    }
}

#[tokio::test]
async fn a_finished_control_stream_is_a_closed_critical_stream() {
    let (_fx, mut raw) = setup(Config::default()).await;
    raw.control.finish().unwrap();
    expect_close(&raw, 0x104).await;
}

#[tokio::test]
async fn a_reset_control_stream_is_a_closed_critical_stream() {
    let (_fx, mut raw) = setup(Config::default()).await;
    answered(&raw).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    raw.control.reset(0u32.into()).unwrap();
    expect_close(&raw, 0x104).await;
}

#[tokio::test]
async fn a_second_control_stream_is_a_stream_creation_error() {
    let (_fx, raw) = setup(Config::default()).await;
    let _second = raw.uni(0x00, &settings_frame()).await;
    expect_close(&raw, 0x103).await;
}

#[tokio::test]
async fn a_client_push_stream_is_a_stream_creation_error() {
    let (_fx, raw) = setup(Config::default()).await;
    let _push = raw.uni(0x01, &[0]).await;
    expect_close(&raw, 0x103).await;
}

#[tokio::test]
async fn a_second_qpack_encoder_stream_is_a_stream_creation_error() {
    let (_fx, raw) = setup(Config::default()).await;
    let _a = raw.uni(0x02, &[]).await;
    let _b = raw.uni(0x02, &[]).await;
    expect_close(&raw, 0x103).await;
}

#[tokio::test]
async fn goaway_may_not_increase() {
    let (_fx, mut raw) = setup(Config::default()).await;
    let both = [
        frame_bytes(frame::GOAWAY, &[8]),
        frame_bytes(frame::GOAWAY, &[12]),
    ]
    .concat();
    raw.control.write_all(&both).await.unwrap();
    expect_close(&raw, 0x108).await;
}

#[tokio::test]
async fn max_push_id_may_not_decrease() {
    let (_fx, mut raw) = setup(Config::default()).await;
    raw.control
        .write_all(&frame_bytes(frame::MAX_PUSH_ID, &[8]))
        .await
        .unwrap();
    raw.control
        .write_all(&frame_bytes(frame::MAX_PUSH_ID, &[4]))
        .await
        .unwrap();
    expect_close(&raw, 0x108).await;
}

#[tokio::test]
async fn cancelling_a_push_that_was_never_promised_is_an_id_error() {
    let (_fx, mut raw) = setup(Config::default()).await;
    raw.control
        .write_all(&frame_bytes(frame::CANCEL_PUSH, &[0]))
        .await
        .unwrap();
    expect_close(&raw, 0x108).await;
}

#[tokio::test]
async fn unknown_stream_types_and_unknown_control_frames_are_tolerated() {
    let (_fx, mut raw) = setup(Config::default()).await;
    let _grease = raw.uni(0x1f * 3 + 0x21, &[1, 2, 3]).await;
    let _unknown = raw.uni(0x40, &[9; 100]).await;
    raw.control
        .write_all(&frame_bytes(0x1f * 2 + 0x21, &[0; 1000]))
        .await
        .unwrap();
    raw.control
        .write_all(&frame_bytes(frame::MAX_PUSH_ID, &[3]))
        .await
        .unwrap();
    // a stream that is closed before its type arrives
    let mut early = raw.conn.open_uni().await.unwrap();
    early.finish().unwrap();
    answered(&raw).await;
    assert!(raw.conn.close_reason().is_none());
}

#[tokio::test]
async fn qpack_streams_accept_only_what_a_table_of_capacity_zero_allows() {
    // set capacity 0 is fine, repeated; the decoder stream may cancel streams
    let (_fx, raw) = setup(Config::default()).await;
    let _encoder = raw.uni(0x02, &[0x20, 0x20, 0x20]).await;
    let _decoder = raw.uni(0x03, &[0x40, 0x41, 0x7f, 0x80, 0x01]).await;
    answered(&raw).await;
    assert!(raw.conn.close_reason().is_none());

    // set capacity above 0, insert with name reference, insert with literal name, duplicate
    for instruction in [
        vec![0x3f, 0x01],
        vec![0xc0, 0x01, b'a'],
        vec![0x41, b'a', 0x01, b'b'],
        vec![0x00],
    ] {
        let (_fx, raw) = setup(Config::default()).await;
        let _encoder = raw.uni(0x02, &instruction).await;
        expect_close(&raw, 0x201).await;
    }
    // insert count increment and section acknowledgment have nothing to refer to
    for instruction in [vec![0x01], vec![0x81]] {
        let (_fx, raw) = setup(Config::default()).await;
        let _decoder = raw.uni(0x03, &instruction).await;
        expect_close(&raw, 0x202).await;
    }
    // a critical stream closed
    let (_fx, raw) = setup(Config::default()).await;
    let mut encoder = raw.uni(0x02, &[]).await;
    encoder.finish().unwrap();
    expect_close(&raw, 0x104).await;
}

#[tokio::test]
async fn request_streams_reset_in_a_flood_close_the_connection() {
    let config = Config {
        reset_flood_limit: 20,
        reset_flood_window: Duration::from_secs(60),
        ..Config::default()
    };
    let fx = fixture();
    fx.serve(config, |mut stream: RequestStream| async move {
        let _ = stream.read_head().await;
        // answer slowly: the peer resets before the response is finished
        let _ = stream.recv_data().await;
    });
    let conn = fx
        .client
        .connect(fx.addr, "localhost")
        .unwrap()
        .await
        .unwrap();
    let raw = Raw::new(conn).await;
    for _ in 0..60 {
        let Ok((mut send, recv)) = raw.conn.open_bi().await else {
            break;
        };
        let _ = send.write_all(&request_bytes(&[])).await;
        let _ = send.reset(0x10cu32.into());
        drop(recv);
        if raw.conn.close_reason().is_some() {
            break;
        }
    }
    expect_close(&raw, 0x107).await;
}

#[tokio::test]
async fn resets_below_the_flood_limit_keep_the_connection() {
    let config = Config {
        reset_flood_limit: 100,
        ..Config::default()
    };
    let fx = fixture();
    fx.serve(config, |mut stream: RequestStream| async move {
        let _ = stream.read_head().await;
        let _ = stream.recv_data().await;
    });
    let conn = fx
        .client
        .connect(fx.addr, "localhost")
        .unwrap()
        .await
        .unwrap();
    let raw = Raw::new(conn).await;
    for _ in 0..30 {
        let (mut send, _recv) = raw.conn.open_bi().await.unwrap();
        send.write_all(&request_bytes(&[])).await.unwrap();
        send.reset(0x10cu32.into()).unwrap();
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(raw.conn.close_reason().is_none());
}

#[tokio::test]
async fn data_after_the_head_is_delivered_in_order_and_a_peer_fin_ends_it() {
    let fx = fixture();
    fx.serve(Config::default(), |mut stream: RequestStream| async move {
        stream.read_head().await.unwrap();
        let mut total = 0;
        while let Some(piece) = stream.recv_data().await.unwrap() {
            total += piece.len();
        }
        let response = Response::builder()
            .header("x-total", total.to_string())
            .body(())
            .unwrap();
        stream
            .send_full_response(response, Bytes::new(), false)
            .await
            .unwrap();
    });
    let conn = fx
        .client
        .connect(fx.addr, "localhost")
        .unwrap()
        .await
        .unwrap();
    let raw = Raw::new(conn).await;
    let mut bytes = request_bytes(&[]);
    for _ in 0..5 {
        bytes.extend(frame_bytes(frame::DATA, &[1; 1000]));
    }
    let (mut send, mut recv) = raw.request(&bytes).await;
    send.finish().unwrap();
    let frames = within(read_frames(&mut recv)).await.unwrap();
    assert_eq!(status_of(&frames[0].1), 200);
    assert_eq!(frames.len(), 1);
}

#[tokio::test]
async fn closing_without_error_ends_accept_with_none() {
    let fx = fixture();
    let (client, server) = fx.connect().await;
    let mut h3 = oxiroute_h3::server::Connection::new(server, Config::default())
        .await
        .unwrap();
    client.close(0x100u32.into(), b"bye");
    assert!(within(h3.accept()).await.unwrap().is_none());
}
