//! Shared robustness harness: feeds arbitrary bytes into the sans-IO connection, exercises accepted streams
//! through the poll-style API and checks that whatever the connection writes parses as frames. Used by
//! `tests/robustness.rs` and (through `#[path]`) by the `h2_frames` fuzz target.
#![allow(dead_code)]
use std::task::{Context, Poll, Waker};

use bytes::{Bytes, BytesMut};
use http::{HeaderMap, StatusCode};
use oxiroute_h2::Reason;
use oxiroute_h2::conn::{Connection, Output, empty_slices};
use oxiroute_h2::server::Config;

const MAX_SEGMENT: usize = 1 << 16;

fn config(variant: u8) -> Config {
    let mut c = Config::default();
    c.max_concurrent_streams(2 + u32::from(variant & 7))
        .max_header_list_size(2048 << (variant & 3))
        .initial_window_size(1024 << (variant & 3))
        .initial_connection_window_size(4096 << (variant >> 2 & 3))
        .max_pending_accept_reset_streams(2)
        .max_local_error_reset_streams(16)
        .max_peer_reset_streams(8)
        .max_continuation_frames(3)
        .max_pending_control_frames(64)
        .max_ignored_frames(64)
        .max_empty_data_frames(8)
        .data_frame_budget(2048);
    c
}

/// Drains the output and checks that it is a sequence of whole frames with sane lengths and types.
fn check_output(conn: &mut Connection, sink: &mut Output) {
    conn.take_output(sink);
    let mut wire = Vec::new();
    while !sink.is_empty() {
        let mut slices = empty_slices();
        let n = sink.slices(&mut slices);
        let before = wire.len();
        for s in &slices[..n] {
            wire.extend_from_slice(s);
        }
        sink.advance(wire.len() - before);
    }
    let mut rest = &wire[..];
    while !rest.is_empty() {
        assert!(rest.len() >= 9, "truncated frame header in output");
        let len = usize::from(rest[0]) << 16 | usize::from(rest[1]) << 8 | usize::from(rest[2]);
        assert!(rest[3] <= 9, "unknown frame type {} in output", rest[3]);
        assert!(rest.len() >= 9 + len, "truncated frame in output");
        assert!(rest[5] & 0x80 == 0, "reserved bit set");
        rest = &rest[9 + len..];
    }
}

/// Exercises one accepted stream; the choice of operations follows `mode`.
fn exercise(conn: &mut Connection, idx: u32, id: u32, mode: u8, cx: &Context<'_>) {
    let _ = conn.is_end_stream(idx, id);
    for _ in 0..4 {
        if let Poll::Ready(Some(Ok(chunk))) = conn.poll_data(idx, id, cx) {
            conn.release_capacity(idx, id, chunk.len());
        }
    }
    let _ = conn.poll_trailers(idx, id, cx);
    let _ = conn.poll_reset(idx, id, cx);
    match mode % 5 {
        0 => {
            let _ = conn.send_response(idx, id, StatusCode::OK, &HeaderMap::new(), true);
        }
        1 => {
            let mut h = HeaderMap::new();
            h.insert("x-request-id", "abc".parse().unwrap());
            h.append("set-cookie", "a=b".parse().unwrap());
            if conn
                .send_response(idx, id, StatusCode::OK, &h, false)
                .is_ok()
            {
                if let Poll::Ready(Ok(n)) = conn.poll_capacity(idx, id, cx, 70_000) {
                    let _ = conn.send_data(idx, id, Bytes::from(vec![7; n]), mode & 8 == 0);
                }
                let _ = conn.send_trailers(idx, id, &HeaderMap::new());
            }
        }
        2 => conn.send_reset(idx, id, Reason::CANCEL),
        3 => {
            let _ = conn.send_response(idx, id, StatusCode::NOT_FOUND, &HeaderMap::new(), false);
            let _ = conn.poll_capacity(idx, id, cx, 0);
            let _ = conn.send_data(idx, id, Bytes::new(), true);
        }
        _ => {}
    }
    if mode & 16 == 0 {
        conn.drop_stream(idx, id);
    }
}

/// `data[0]`: bit 0 prepends an empty SETTINGS frame, bits 1..5 pick the configuration; `data[1]` the chunk size.
/// Returns the number of requests accepted.
pub fn run(data: &[u8]) -> usize {
    let [flags, chunk, rest @ ..] = data else {
        return 0;
    };
    let mut accepted = 0;
    let mut conn = Connection::new(config(flags >> 1));
    let mut sink = Output::default();
    let mut input = BytesMut::new();
    let mut held = Vec::new();
    let mut script = Vec::new();
    if flags & 1 != 0 {
        script.extend_from_slice(&[0, 0, 0, 4, 0, 0, 0, 0, 0]);
    }
    script.extend_from_slice(rest);
    let waker = Waker::noop();
    let cx = Context::from_waker(waker);
    let size = 1 + usize::from(*chunk) * 3;
    for (i, part) in script.chunks(size.min(MAX_SEGMENT)).enumerate() {
        input.extend_from_slice(part);
        conn.feed(&mut input);
        while let Some((idx, id, parts)) = conn.pop_accept() {
            assert_eq!(parts.version, http::Version::HTTP_2);
            accepted += 1;
            held.push((idx, id));
            if held.len() > 4 {
                let (idx, id) = held.remove(0);
                exercise(&mut conn, idx, id, flags.wrapping_add(i as u8), &cx);
            }
        }
        for &(idx, id) in &held {
            let _ = conn.poll_data(idx, id, &cx);
        }
        check_output(&mut conn, &mut sink);
        if i == 2 && flags & 0x40 != 0 {
            conn.graceful_shutdown();
        }
    }
    for (n, (idx, id)) in held.into_iter().enumerate() {
        exercise(&mut conn, idx, id, flags.wrapping_add(n as u8), &cx);
    }
    conn.graceful_shutdown();
    conn.abrupt_shutdown(Reason::NO_ERROR);
    check_output(&mut conn, &mut sink);
    conn.transport_closed(None);
    let _ = (conn.closed().is_some(), conn.is_drained(), conn.needed());
    let _ = conn.take_report();
    accepted
}
