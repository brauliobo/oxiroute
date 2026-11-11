//! Raw-frame tests: drive the server with hand-built frames and assert the observable reaction (`GOAWAY` code,
//! `RST_STREAM` code or response status). Paused time makes `Wire::settle` a deterministic quiescence barrier.
//! This file: basics, rapid reset, local resets, CONTINUATION floods, HPACK, control-frame, DATA and PRIORITY floods.
#![allow(
    clippy::cast_possible_truncation,
    clippy::doc_markdown,
    clippy::too_many_lines
)]
#![allow(clippy::many_single_char_names)]
mod raw_rig;
mod support;
use std::time::Duration;

use oxiroute_h2::Reason;
use oxiroute_h2::server::Config;
use raw_rig::*;
use support::Reaction::{GoAway, Reset, Status};
use support::*;

// ---- basics ----

#[tokio::test(start_paused = true)]
async fn a_get_is_answered() {
    let mut r = start(Config::default(), Mode::Respond).await;
    let mut c = Client::new();
    assert_eq!(r.exchange(&c.get(1, "/")).await, vec![Status(1, 200)]);
}

#[tokio::test(start_paused = true)]
async fn unknown_frame_types_are_ignored() {
    let mut r = start(Config::default(), Mode::Respond).await;
    let mut c = Client::new();
    let out = r
        .exchange(&concat(&[
            frame(0xfa, 0, 0, b"junk"),
            frame(0xfb, 0xff, 3, &[]),
            c.get(1, "/"),
        ]))
        .await;
    assert_eq!(out, vec![Status(1, 200)]);
}

#[tokio::test(start_paused = true)]
async fn the_first_frame_must_be_settings() {
    let mut r = rig(Config::default(), Mode::Respond, 1 << 20);
    r.wire
        .send(&concat(&[PREFACE.to_vec(), ping([0; 8], false)]))
        .await;
    assert_eq!(r.settle().await, vec![GoAway(PROTOCOL)]);
    assert!(r.task.await.unwrap().unwrap_err().is_go_away());
}

// ---- rapid reset (CVE-2023-44487) ----

fn get_and_reset(c: &mut Client, n: u32) -> Vec<u8> {
    (0..n)
        .flat_map(|i| [c.get(2 * i + 1, "/"), rst(2 * i + 1, 8)].concat())
        .collect()
}

#[tokio::test(start_paused = true)]
async fn resets_of_streams_the_application_has_not_accepted_are_limited() {
    let mut r = start(Config::default(), Mode::Respond).await;
    let mut c = Client::new();
    let out = r.exchange(&get_and_reset(&mut c, 25)).await;
    assert_eq!(out, vec![GoAway(CALM)]);
    assert_eq!(
        r.task.await.unwrap().unwrap_err().reason(),
        Some(Reason::ENHANCE_YOUR_CALM)
    );
}

#[tokio::test(start_paused = true)]
async fn resets_within_the_pending_limit_are_tolerated_and_never_delivered() {
    let mut r = start(Config::default(), Mode::Respond).await;
    let mut c = Client::new();
    assert_eq!(r.exchange(&get_and_reset(&mut c, 20)).await, vec![]);
    // The tombstones were dequeued, so the budget is free again.
    assert_eq!(r.exchange(&get_and_reset(&mut c, 0)).await, vec![]);
    assert_eq!(r.exchange(&c.get(41, "/")).await, vec![Status(41, 200)]);
    assert_eq!(
        r.exchange(&get_and_reset_from(&mut c, 43, 20)).await,
        vec![]
    );
}

fn get_and_reset_from(c: &mut Client, first: u32, n: u32) -> Vec<u8> {
    (0..n)
        .flat_map(|i| [c.get(first + 2 * i, "/"), rst(first + 2 * i, 8)].concat())
        .collect()
}

#[tokio::test(start_paused = true)]
async fn resets_of_accepted_streams_beyond_the_budget_end_the_connection() {
    let mut r = start(
        cfg(|c| {
            c.max_peer_reset_streams(3);
        }),
        Mode::Hold,
    )
    .await;
    let mut c = Client::new();
    let opens: Vec<u8> = (0..5).flat_map(|i| c.post(2 * i + 1, "/")).collect();
    assert_eq!(r.exchange(&opens).await, vec![]);
    let three = concat(&[rst(1, 8), rst(3, 8), rst(5, 8)]);
    assert_eq!(r.exchange(&three).await, vec![]);
    assert_eq!(r.exchange(&rst(7, 8)).await, vec![GoAway(CALM)]);
}

#[tokio::test(start_paused = true)]
async fn completed_streams_earn_the_reset_budget_back() {
    let mut r = start(
        cfg(|c| {
            c.max_peer_reset_streams(1);
        }),
        Mode::Echo,
    )
    .await;
    let mut c = Client::new();
    let opens = concat(&[c.post(1, "/"), c.post(3, "/")]);
    assert_eq!(r.exchange(&opens).await, vec![]);
    assert_eq!(r.exchange(&rst(1, 8)).await, vec![]);
    // Stream 3 completes normally: one reset is forgiven.
    assert_eq!(
        r.exchange(&data(3, b"x", END_STREAM)).await,
        vec![Status(3, 200)]
    );
    assert_eq!(r.exchange(&c.post(5, "/")).await, vec![]);
    assert_eq!(r.exchange(&rst(5, 8)).await, vec![]);
    // Without a completion in between the next reset is one too many.
    assert_eq!(r.exchange(&c.post(7, "/")).await, vec![]);
    assert_eq!(r.exchange(&rst(7, 8)).await, vec![GoAway(CALM)]);
}

// ---- local error resets ----

fn bad_requests(c: &mut Client, n: u32) -> Vec<u8> {
    (0..n)
        .flat_map(|i| {
            let b = c.block(&[
                (":method", "GET"),
                (":scheme", "https"),
                (":authority", "example.test"),
            ]);
            headers(2 * i + 1, &b, END_STREAM | END_HEADERS)
        })
        .collect()
}

#[tokio::test(start_paused = true)]
async fn stream_errors_are_answered_with_a_reset_until_the_local_budget_is_spent() {
    let mut r = start(
        cfg(|c| {
            c.max_local_error_reset_streams(3);
        }),
        Mode::Respond,
    )
    .await;
    let mut c = Client::new();
    let out = r.exchange(&bad_requests(&mut c, 3)).await;
    assert_eq!(
        out,
        vec![Reset(1, PROTOCOL), Reset(3, PROTOCOL), Reset(5, PROTOCOL)]
    );
}

#[tokio::test(start_paused = true)]
async fn too_many_local_error_resets_end_the_connection() {
    let mut r = start(
        cfg(|c| {
            c.max_local_error_reset_streams(3);
        }),
        Mode::Respond,
    )
    .await;
    let mut c = Client::new();
    let out = r.exchange(&bad_requests(&mut c, 4)).await;
    assert_eq!(
        out,
        vec![
            GoAway(CALM),
            Reset(1, 1),
            Reset(3, 1),
            Reset(5, 1),
            Reset(7, 1)
        ]
    );
}

// ---- CONTINUATION flood (CVE-2024-27316) ----

#[tokio::test(start_paused = true)]
async fn a_header_block_split_over_continuation_frames_is_decoded() {
    let mut r = start(Config::default(), Mode::Respond).await;
    let mut c = Client::new();
    let b = c.get_block("/split");
    let (a, rest) = b.split_at(b.len() / 2);
    let (m, z) = rest.split_at(rest.len() / 2);
    let wire = concat(&[
        headers(1, a, END_STREAM),
        continuation(1, m, 0),
        continuation(1, z, END_HEADERS),
    ]);
    assert_eq!(r.exchange(&wire).await, vec![Status(1, 200)]);
}

#[tokio::test(start_paused = true)]
async fn too_many_continuation_frames_end_the_connection() {
    let mut r = start(Config::default(), Mode::Respond).await;
    let mut wire = headers(1, &[0x82], 0);
    for _ in 0..10 {
        wire.extend(continuation(1, &[0x82], 0));
    }
    assert_eq!(r.exchange(&wire).await, vec![GoAway(CALM)]);
}

#[tokio::test(start_paused = true)]
async fn an_oversized_header_block_ends_the_connection() {
    let mut r = start(
        cfg(|c| {
            c.max_header_block_bytes(100);
        }),
        Mode::Respond,
    )
    .await;
    let wire = concat(&[
        headers(1, &[0x82; 60], 0),
        continuation(1, &[0x82; 60], END_HEADERS),
    ]);
    assert_eq!(r.exchange(&wire).await, vec![GoAway(CALM)]);
}

#[tokio::test(start_paused = true)]
async fn another_frame_inside_a_header_block_is_a_protocol_error() {
    for other in [
        ping([0; 8], false),
        data(3, b"x", 0),
        continuation(3, &[0x82], END_HEADERS),
        headers(3, &[0x82], END_HEADERS),
    ] {
        let mut r = start(Config::default(), Mode::Respond).await;
        let out = r.exchange(&concat(&[headers(1, &[0x82], 0), other])).await;
        assert_eq!(out, vec![GoAway(PROTOCOL)]);
    }
}

#[tokio::test(start_paused = true)]
async fn continuation_without_headers_is_a_protocol_error() {
    let mut r = start(Config::default(), Mode::Respond).await;
    assert_eq!(
        r.exchange(&continuation(1, &[0x82], END_HEADERS)).await,
        vec![GoAway(PROTOCOL)]
    );
}

// ---- header list size and HPACK ----

#[tokio::test(start_paused = true)]
async fn a_request_over_the_header_list_limit_is_answered_431() {
    let mut r = start(
        cfg(|c| {
            c.max_header_list_size(256);
        }),
        Mode::Respond,
    )
    .await;
    let mut c = Client::new();
    let big = "v".repeat(300);
    let b = c.block(&[
        (":method", "GET"),
        (":scheme", "https"),
        (":authority", "e.test"),
        (":path", "/"),
        ("x-big", &big),
    ]);
    let out = r.exchange(&headers(1, &b, END_STREAM | END_HEADERS)).await;
    // A finished request is closed by the 431 itself (like h2), no RST_STREAM follows.
    assert_eq!(out, vec![Status(1, 431)]);
    // The HPACK state stayed in step: the next request decodes.
    assert_eq!(r.exchange(&c.get(3, "/")).await, vec![Status(3, 200)]);
}

#[tokio::test(start_paused = true)]
async fn an_hpack_bomb_of_indexed_references_is_refused_without_delivery() {
    let mut r = start(
        cfg(|c| {
            c.max_header_list_size(8192);
        }),
        Mode::Respond,
    )
    .await;
    let mut c = Client::new();
    let seed = c.block(&[
        (":method", "GET"),
        (":scheme", "https"),
        (":authority", "e.test"),
        (":path", "/"),
        ("x-bomb", &"b".repeat(3000)),
    ]);
    assert_eq!(
        r.exchange(&headers(1, &seed, END_STREAM | END_HEADERS))
            .await,
        vec![Status(1, 200)]
    );
    // 200 one-octet references to the 3 KB entry decode to 600 KB.
    let bomb = headers(3, &[0xBE; 200], END_STREAM | END_HEADERS);
    assert_eq!(r.exchange(&bomb).await, vec![Status(3, 431)]);
}

#[tokio::test(start_paused = true)]
async fn invalid_hpack_is_a_compression_error() {
    let mut r = start(Config::default(), Mode::Respond).await;
    // Index 127 is far beyond the static and the (empty) dynamic table.
    assert_eq!(
        r.exchange(&headers(1, &[0xFF, 0x00], END_STREAM | END_HEADERS))
            .await,
        vec![GoAway(COMPRESSION)]
    );
}

// ---- control-frame floods against a peer that does not read ----

/// Server behind a pipe so small that its replies stay queued; the client never reads until the end.
async fn flood(config: Config, frame_bytes: Vec<u8>) -> Vec<Reaction> {
    let mut r = rig(config, Mode::Respond, 64);
    r.wire.start().await;
    // Once the server gives up it stops reading, so a long send may stay blocked.
    let _ = tokio::time::timeout(Duration::from_secs(1), r.wire.send(&frame_bytes)).await;
    r.settle().await
}

#[tokio::test(start_paused = true)]
async fn a_settings_flood_is_met_with_enhance_your_calm() {
    let out = flood(
        cfg(|c| {
            c.max_pending_control_frames(5);
        }),
        (0..40).flat_map(|_| settings(&[])).collect(),
    )
    .await;
    assert_eq!(out, vec![GoAway(CALM)]);
}

#[tokio::test(start_paused = true)]
async fn a_ping_flood_is_met_with_enhance_your_calm() {
    let out = flood(
        cfg(|c| {
            c.max_pending_control_frames(5);
        }),
        (0..40).flat_map(|_| ping([1; 8], false)).collect(),
    )
    .await;
    assert_eq!(out, vec![GoAway(CALM)]);
}

#[tokio::test(start_paused = true)]
async fn pings_and_settings_are_acknowledged_when_the_peer_reads() {
    let mut r = start(
        cfg(|c| {
            c.max_pending_control_frames(5);
        }),
        Mode::Respond,
    )
    .await;
    for _ in 0..20 {
        r.wire
            .send(&concat(&[ping([2; 8], false), settings(&[])]))
            .await;
        r.settle().await;
    }
    let acks = r
        .wire
        .frames
        .iter()
        .filter(|f| f.kind == PING && f.flags & 1 == 1)
        .count();
    assert_eq!(acks, 20);
}

// ---- DATA and PRIORITY floods ----

#[tokio::test(start_paused = true)]
async fn an_empty_data_flood_is_met_with_enhance_your_calm() {
    let mut r = start(
        cfg(|c| {
            c.max_empty_data_frames(5);
        }),
        Mode::Hold,
    )
    .await;
    let mut c = Client::new();
    let flood: Vec<u8> = (0..10).flat_map(|_| data(1, &[], 0)).collect();
    assert_eq!(
        r.exchange(&concat(&[c.post(1, "/"), flood])).await,
        vec![GoAway(CALM)]
    );
}

#[tokio::test(start_paused = true)]
async fn a_final_empty_data_frame_is_free() {
    let mut r = start(
        cfg(|c| {
            c.max_empty_data_frames(0);
        }),
        Mode::Respond,
    )
    .await;
    let mut c = Client::new();
    assert_eq!(
        r.exchange(&concat(&[c.post(1, "/"), data(1, &[], END_STREAM)]))
            .await,
        vec![Status(1, 200)]
    );
}

#[tokio::test(start_paused = true)]
async fn tiny_data_frames_exhaust_the_overhead_budget() {
    let mut r = start(
        cfg(|c| {
            c.data_frame_budget(1000);
        }),
        Mode::Hold,
    )
    .await;
    let mut c = Client::new();
    let tiny: Vec<u8> = (0..10).flat_map(|_| data(1, b"x", 0)).collect();
    assert_eq!(
        r.exchange(&concat(&[c.post(1, "/"), tiny])).await,
        vec![GoAway(CALM)]
    );
}

#[tokio::test(start_paused = true)]
async fn tiny_data_frames_the_application_reads_return_their_budget() {
    let mut r = start(
        cfg(|c| {
            c.data_frame_budget(1000);
        }),
        Mode::Echo,
    )
    .await;
    let mut c = Client::new();
    r.exchange(&c.post(1, "/")).await;
    for _ in 0..20 {
        assert_eq!(r.exchange(&data(1, b"x", 0)).await, vec![]);
    }
}

#[tokio::test(start_paused = true)]
async fn a_priority_flood_is_met_with_enhance_your_calm() {
    let mut r = start(
        cfg(|c| {
            c.max_ignored_frames(5);
        }),
        Mode::Respond,
    )
    .await;
    let flood: Vec<u8> = (0..10).flat_map(|_| priority(3, 1)).collect();
    assert_eq!(r.exchange(&flood).await, vec![GoAway(CALM)]);
}

#[tokio::test(start_paused = true)]
async fn priority_frames_are_otherwise_ignored() {
    let mut r = start(Config::default(), Mode::Respond).await;
    let mut c = Client::new();
    let wire = concat(&[priority(3, 1), priority(5, 3), c.get(1, "/")]);
    assert_eq!(r.exchange(&wire).await, vec![Status(1, 200)]);
}

#[tokio::test(start_paused = true)]
async fn a_priority_frame_with_the_wrong_length_or_self_dependency_is_a_stream_error() {
    let mut r = start(Config::default(), Mode::Respond).await;
    assert_eq!(
        r.exchange(&frame(2, 0, 3, &[0; 4])).await,
        vec![Reset(3, FRAME_SIZE)]
    );
    assert_eq!(r.exchange(&priority(5, 5)).await, vec![Reset(5, PROTOCOL)]);
    assert_eq!(r.exchange(&priority(0, 1)).await, vec![GoAway(PROTOCOL)]);
}
