//! Raw-frame tests, part 2: concurrency, stream id rules, frame and SETTINGS validity, flow control.
#![allow(
    clippy::cast_possible_truncation,
    clippy::doc_markdown,
    clippy::too_many_lines
)]
#![allow(clippy::many_single_char_names)]
mod raw_rig;
mod support;
use std::time::Duration;

use oxiroute_h2::server::Config;
use raw_rig::*;
use support::Reaction::{GoAway, Reset, Status};
use support::*;

// ---- concurrency ----

#[tokio::test(start_paused = true)]
async fn streams_beyond_the_concurrency_limit_are_refused() {
    let mut r = start(
        cfg(|c| {
            c.max_concurrent_streams(2);
        }),
        Mode::Hold,
    )
    .await;
    let mut c = Client::new();
    let wire = concat(&[c.post(1, "/"), c.post(3, "/"), c.post(5, "/")]);
    assert_eq!(r.exchange(&wire).await, vec![Reset(5, REFUSED)]);
    // The refused stream still advanced the HPACK state and the id space.
    assert_eq!(r.exchange(&c.post(7, "/")).await, vec![Reset(7, REFUSED)]);
    // A reset frees a slot.
    assert_eq!(r.exchange(&rst(1, 8)).await, vec![]);
    assert_eq!(r.exchange(&c.post(9, "/")).await, vec![]);
}

// ---- stream id rules ----

#[tokio::test(start_paused = true)]
async fn client_stream_ids_must_be_odd_and_increasing() {
    let mut r = start(Config::default(), Mode::Respond).await;
    assert_eq!(
        r.exchange(&Client::new().get(2, "/")).await,
        vec![GoAway(PROTOCOL)]
    );
    let mut r = start(Config::default(), Mode::Respond).await;
    let mut c = Client::new();
    let out = r.exchange(&concat(&[c.get(5, "/"), c.get(3, "/")])).await;
    assert_eq!(out, vec![GoAway(PROTOCOL)]);
    let g = r.wire.frames.iter().find(|f| f.kind == GOAWAY).unwrap();
    assert_eq!(
        u32::from_be_bytes(g.payload[..4].try_into().unwrap()),
        5,
        "last stream id"
    );
    let mut r = start(Config::default(), Mode::Respond).await;
    assert_eq!(
        r.exchange(&headers(0, &[0x82], END_HEADERS)).await,
        vec![GoAway(PROTOCOL)]
    );
}

#[tokio::test(start_paused = true)]
async fn headers_on_a_closed_stream_are_a_protocol_error() {
    let mut r = start(Config::default(), Mode::Respond).await;
    let mut c = Client::new();
    assert_eq!(r.exchange(&c.get(1, "/")).await, vec![Status(1, 200)]);
    assert_eq!(r.exchange(&c.get(1, "/")).await, vec![GoAway(PROTOCOL)]);
}

#[tokio::test(start_paused = true)]
async fn trailers_without_end_stream_and_headers_after_end_stream_are_stream_errors() {
    let mut r = start(Config::default(), Mode::Hold).await;
    let mut c = Client::new();
    r.exchange(&concat(&[c.post(1, "/"), c.post(3, "/")])).await;
    let t = c.block(&[("x-t", "1")]);
    assert_eq!(
        r.exchange(&headers(1, &t, END_HEADERS)).await,
        vec![Reset(1, PROTOCOL)]
    );
    r.exchange(&data(3, b"x", END_STREAM)).await;
    let t = c.block(&[("x-t", "1")]);
    assert_eq!(
        r.exchange(&headers(3, &t, END_STREAM | END_HEADERS)).await,
        vec![Reset(3, STREAM_CLOSED)]
    );
}

#[tokio::test(start_paused = true)]
async fn data_on_an_idle_stream_is_a_protocol_error() {
    let mut r = start(Config::default(), Mode::Respond).await;
    assert_eq!(r.exchange(&data(9, b"x", 0)).await, vec![GoAway(PROTOCOL)]);
}

#[tokio::test(start_paused = true)]
async fn data_on_a_closed_stream_is_a_stream_closed_error() {
    let mut r = start(Config::default(), Mode::Respond).await;
    let mut c = Client::new();
    assert_eq!(r.exchange(&c.get(1, "/")).await, vec![Status(1, 200)]);
    assert_eq!(
        r.exchange(&data(1, b"x", 0)).await,
        vec![Reset(1, STREAM_CLOSED)]
    );
}

#[tokio::test(start_paused = true)]
async fn data_after_end_stream_on_an_open_stream_is_a_stream_closed_error() {
    let mut r = start(Config::default(), Mode::Hold).await;
    let mut c = Client::new();
    r.exchange(&c.post(1, "/")).await;
    assert_eq!(r.exchange(&data(1, b"x", END_STREAM)).await, vec![]);
    assert_eq!(
        r.exchange(&data(1, b"y", 0)).await,
        vec![Reset(1, STREAM_CLOSED)]
    );
}

#[tokio::test(start_paused = true)]
async fn frames_in_flight_for_a_stream_the_server_reset_are_ignored() {
    let mut r = start(Config::default(), Mode::Respond).await;
    let mut c = Client::new();
    // The answer to the POST resets it (NO_ERROR) while the client is still sending its body.
    let out = r.exchange(&c.post(1, "/")).await;
    assert_eq!(out, vec![Reset(1, 0), Status(1, 200)]);
    assert_eq!(
        r.exchange(&concat(&[data(1, b"late", 0), data(1, b"", END_STREAM)]))
            .await,
        vec![]
    );
}

#[tokio::test(start_paused = true)]
async fn reset_and_window_update_on_idle_streams_are_protocol_errors() {
    let mut r = start(Config::default(), Mode::Respond).await;
    assert_eq!(r.exchange(&rst(7, 8)).await, vec![GoAway(PROTOCOL)]);
    let mut r = start(Config::default(), Mode::Respond).await;
    assert_eq!(
        r.exchange(&window_update(7, 1)).await,
        vec![GoAway(PROTOCOL)]
    );
}

#[tokio::test(start_paused = true)]
async fn reset_and_window_update_on_closed_streams_are_ignored() {
    let mut r = start(Config::default(), Mode::Respond).await;
    let mut c = Client::new();
    assert_eq!(r.exchange(&c.get(1, "/")).await, vec![Status(1, 200)]);
    assert_eq!(
        r.exchange(&concat(&[rst(1, 8), window_update(1, 5)])).await,
        vec![]
    );
    assert_eq!(r.exchange(&c.get(3, "/")).await, vec![Status(3, 200)]);
}

// ---- frame and SETTINGS validity ----

#[tokio::test(start_paused = true)]
async fn a_frame_over_the_advertised_size_is_a_frame_size_error() {
    let mut r = start(Config::default(), Mode::Respond).await;
    let mut f = frame(DATA, 0, 1, &[]);
    f[..3].copy_from_slice(&[0, 0x40, 0x01]); // 16385
    assert_eq!(r.exchange(&f).await, vec![GoAway(FRAME_SIZE)]);
}

#[tokio::test(start_paused = true)]
async fn a_larger_configured_frame_size_is_accepted_up_to_its_limit() {
    let mut r = start(
        cfg(|c| {
            c.max_frame_size(32_768);
        }),
        Mode::Echo,
    )
    .await;
    let mut c = Client::new();
    r.exchange(&c.post(1, "/")).await;
    let out = r.exchange(&data(1, &vec![7; 32_768], END_STREAM)).await;
    assert_eq!(out, vec![Status(1, 200)]);
    let mut f = frame(DATA, 0, 3, &[]);
    f[..3].copy_from_slice(&[0, 0x80, 0x01]); // 32769
    assert_eq!(r.exchange(&f).await, vec![GoAway(FRAME_SIZE)]);
}

#[tokio::test(start_paused = true)]
async fn invalid_settings_are_connection_errors() {
    let cases: [(&[(u16, u32)], u32); 5] = [
        (&[(2, 2)], PROTOCOL),
        (&[(4, 1 << 31)], FLOW_CONTROL),
        (&[(5, 16_383)], PROTOCOL),
        (&[(5, 1 << 24)], PROTOCOL),
        (&[(2, 1), (4, 1 << 31)], FLOW_CONTROL),
    ];
    for (entries, code) in cases {
        let mut r = start(Config::default(), Mode::Respond).await;
        assert_eq!(
            r.exchange(&settings(entries)).await,
            vec![GoAway(code)],
            "{entries:?}"
        );
    }
    let mut r = start(Config::default(), Mode::Respond).await;
    assert_eq!(
        r.exchange(&frame(SETTINGS, 0, 0, &[0; 5])).await,
        vec![GoAway(FRAME_SIZE)]
    );
    let mut r = start(Config::default(), Mode::Respond).await;
    assert_eq!(
        r.exchange(&frame(SETTINGS, 0, 1, &[0; 6])).await,
        vec![GoAway(PROTOCOL)]
    );
}

#[tokio::test(start_paused = true)]
async fn a_settings_ack_with_a_payload_is_a_frame_size_error() {
    let mut r = start(Config::default(), Mode::Respond).await;
    assert_eq!(
        r.exchange(&frame(SETTINGS, 1, 0, &[0; 6])).await,
        vec![GoAway(FRAME_SIZE)]
    );
}

#[tokio::test(start_paused = true)]
async fn an_unexpected_settings_ack_is_a_protocol_error() {
    let mut r = start(Config::default(), Mode::Respond).await;
    assert_eq!(r.exchange(&settings_ack()).await, vec![GoAway(PROTOCOL)]);
}

#[tokio::test(start_paused = true)]
async fn a_peer_that_never_acknowledges_our_settings_times_out() {
    let mut r = rig(
        cfg(|c| {
            c.settings_ack_timeout(Duration::from_secs(5));
        }),
        Mode::Respond,
        1 << 20,
    );
    r.wire
        .send(&concat(&[PREFACE.to_vec(), settings(&[])]))
        .await;
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert_eq!(r.settle().await, vec![]);
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(r.settle().await, vec![GoAway(SETTINGS_TIMEOUT)]);
}

#[tokio::test(start_paused = true)]
async fn an_acknowledged_settings_frame_does_not_time_out() {
    let mut r = start(
        cfg(|c| {
            c.settings_ack_timeout(Duration::from_secs(5));
        }),
        Mode::Respond,
    )
    .await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    let mut c = Client::new();
    assert_eq!(r.exchange(&c.get(1, "/")).await, vec![Status(1, 200)]);
}

#[tokio::test(start_paused = true)]
async fn our_advertised_settings_follow_the_configuration() {
    let config = cfg(|c| {
        c.max_concurrent_streams(7)
            .max_header_list_size(4000)
            .initial_window_size(1000);
        c.initial_connection_window_size(1 << 20)
            .max_frame_size(20_000)
            .header_table_size(2048);
    });
    let mut r = rig(config, Mode::Respond, 1 << 20);
    r.wire.start().await;
    let frames = r.wire.settle().await;
    let s = &frames[0];
    assert_eq!(s.kind, SETTINGS);
    let entries: Vec<(u16, u32)> = s
        .payload
        .chunks(6)
        .map(|e| {
            (
                u16::from_be_bytes([e[0], e[1]]),
                u32::from_be_bytes([e[2], e[3], e[4], e[5]]),
            )
        })
        .collect();
    assert_eq!(
        entries,
        vec![(1, 2048), (3, 7), (4, 1000), (5, 20_000), (6, 4000)]
    );
    let wu = frames
        .iter()
        .find(|f| f.kind == WINDOW_UPDATE)
        .expect("connection window update");
    assert_eq!(wu.stream, 0);
    assert_eq!(
        u32::from_be_bytes(wu.payload[..4].try_into().unwrap()),
        (1 << 20) - 65_535
    );
}

#[tokio::test(start_paused = true)]
async fn push_promise_from_a_client_is_a_protocol_error() {
    let mut r = start(Config::default(), Mode::Respond).await;
    assert_eq!(
        r.exchange(&frame(5, END_HEADERS, 1, &[0, 0, 0, 2, 0x82]))
            .await,
        vec![GoAway(PROTOCOL)]
    );
}

#[tokio::test(start_paused = true)]
async fn ping_and_goaway_frames_are_validated() {
    let mut r = start(Config::default(), Mode::Respond).await;
    assert_eq!(
        r.exchange(&frame(PING, 0, 1, &[0; 8])).await,
        vec![GoAway(PROTOCOL)]
    );
    let mut r = start(Config::default(), Mode::Respond).await;
    assert_eq!(
        r.exchange(&frame(PING, 0, 0, &[0; 7])).await,
        vec![GoAway(FRAME_SIZE)]
    );
    let mut r = start(Config::default(), Mode::Respond).await;
    assert_eq!(
        r.exchange(&frame(GOAWAY, 0, 1, &[0; 8])).await,
        vec![GoAway(PROTOCOL)]
    );
    let mut r = start(Config::default(), Mode::Respond).await;
    assert_eq!(
        r.exchange(&frame(GOAWAY, 0, 0, &[0; 4])).await,
        vec![GoAway(FRAME_SIZE)]
    );
    let mut r = start(Config::default(), Mode::Respond).await;
    assert_eq!(
        r.exchange(&frame(RST_STREAM, 0, 0, &[0; 4])).await,
        vec![GoAway(PROTOCOL)]
    );
    let mut r = start(Config::default(), Mode::Respond).await;
    assert_eq!(
        r.exchange(&frame(RST_STREAM, 0, 1, &[0; 3])).await,
        vec![GoAway(FRAME_SIZE)]
    );
    let mut r = start(Config::default(), Mode::Respond).await;
    assert_eq!(
        r.exchange(&frame(WINDOW_UPDATE, 0, 0, &[0; 3])).await,
        vec![GoAway(FRAME_SIZE)]
    );
    let mut r = start(Config::default(), Mode::Respond).await;
    assert_eq!(
        r.exchange(&frame(DATA, 0, 0, b"x")).await,
        vec![GoAway(PROTOCOL)]
    );
}

#[tokio::test(start_paused = true)]
async fn a_ping_is_echoed_and_a_client_goaway_drains_the_connection() {
    let mut r = start(Config::default(), Mode::Respond).await;
    r.exchange(&ping(*b"12345678", false)).await;
    let ack = r.wire.frames.iter().find(|f| f.kind == PING).expect("ack");
    assert_eq!(
        (ack.flags, ack.payload.as_slice()),
        (1, b"12345678".as_slice())
    );
    r.exchange(&goaway(0, 0)).await;
    assert!(r.task.await.unwrap().is_ok());
}

// ---- flow control ----

#[tokio::test(start_paused = true)]
async fn data_beyond_the_stream_window_is_a_flow_control_stream_error() {
    let mut r = start(
        cfg(|c| {
            c.initial_window_size(100);
        }),
        Mode::Hold,
    )
    .await;
    let mut c = Client::new();
    assert_eq!(
        r.exchange(&concat(&[c.post(1, "/"), data(1, &[0; 100], 0)]))
            .await,
        vec![]
    );
    assert_eq!(
        r.exchange(&data(1, b"x", 0)).await,
        vec![Reset(1, FLOW_CONTROL)]
    );
}

#[tokio::test(start_paused = true)]
async fn data_beyond_the_connection_window_is_a_flow_control_connection_error() {
    let mut r = start(Config::default(), Mode::Hold).await;
    let mut c = Client::new();
    r.exchange(&concat(&[c.post(1, "/"), c.post(3, "/")])).await;
    let fill = concat(&[
        data(1, &[0; 16_384], 0),
        data(1, &[0; 16_384], 0),
        data(1, &[0; 16_384], 0),
        data(1, &[0; 16_383], 0),
    ]);
    assert_eq!(r.exchange(&fill).await, vec![]);
    assert_eq!(
        r.exchange(&data(3, b"x", 0)).await,
        vec![GoAway(FLOW_CONTROL)]
    );
}

#[tokio::test(start_paused = true)]
async fn consumed_request_body_is_credited_back_in_batches() {
    let mut r = start(Config::default(), Mode::Echo).await;
    let mut c = Client::new();
    r.exchange(&c.post(1, "/")).await;
    let chunk = vec![1; 16_384];
    r.exchange(&concat(&[data(1, &chunk, 0), data(1, &chunk, 0)]))
        .await;
    let updates: Vec<_> = r
        .wire
        .frames
        .iter()
        .filter(|f| f.kind == WINDOW_UPDATE)
        .collect();
    let total = |stream| {
        updates
            .iter()
            .filter(|f| f.stream == stream)
            .map(|f| u32::from_be_bytes(f.payload[..4].try_into().unwrap()))
            .sum::<u32>()
    };
    assert_eq!((total(0), total(1)), (32_768, 32_768));
    assert_eq!(updates.len(), 2, "one batched update per window");
}

#[tokio::test(start_paused = true)]
async fn window_update_overflow_is_a_flow_control_error() {
    let mut r = start(Config::default(), Mode::Hold).await;
    let mut c = Client::new();
    r.exchange(&c.post(1, "/")).await;
    assert_eq!(
        r.exchange(&window_update(1, 0x7fff_ffff)).await,
        vec![Reset(1, FLOW_CONTROL)]
    );
    assert_eq!(
        r.exchange(&window_update(0, 0x7fff_ffff)).await,
        vec![GoAway(FLOW_CONTROL)]
    );
}

#[tokio::test(start_paused = true)]
async fn a_zero_window_update_is_a_protocol_error() {
    let mut r = start(Config::default(), Mode::Hold).await;
    let mut c = Client::new();
    r.exchange(&c.post(1, "/")).await;
    assert_eq!(
        r.exchange(&window_update(1, 0)).await,
        vec![Reset(1, PROTOCOL)]
    );
    assert_eq!(
        r.exchange(&window_update(0, 0)).await,
        vec![GoAway(PROTOCOL)]
    );
}

fn data_frames(r: &Rig, from: usize) -> Vec<(usize, bool)> {
    r.wire.frames[from..]
        .iter()
        .filter(|f| f.kind == DATA)
        .map(|f| (f.payload.len(), f.flags & 1 == 1))
        .collect()
}

#[tokio::test(start_paused = true)]
async fn the_send_window_follows_initial_window_size_changes_through_negative_values() {
    let mut r = rig(Config::default(), Mode::Body(100), 1 << 20);
    r.wire
        .send(&concat(&[
            PREFACE.to_vec(),
            settings(&[(4, 10)]),
            settings_ack(),
        ]))
        .await;
    let mut c = Client::new();
    r.exchange(&c.get(1, "/")).await;
    assert_eq!(data_frames(&r, 0), vec![(10, false)]);
    let seen = r.wire.frames.len();
    // The 10 sent octets are now beyond a window of 0: -10, then -5.
    r.exchange(&settings(&[(4, 0)])).await;
    r.exchange(&settings(&[(4, 5)])).await;
    assert_eq!(data_frames(&r, seen), vec![]);
    r.exchange(&window_update(1, 5)).await;
    assert_eq!(
        data_frames(&r, seen),
        vec![],
        "window is back to zero, not positive"
    );
    r.exchange(&window_update(1, 90)).await;
    assert_eq!(data_frames(&r, seen), vec![(90, true)]);
}

#[tokio::test(start_paused = true)]
async fn an_initial_window_size_that_overflows_a_stream_window_is_a_flow_control_error() {
    let mut r = start(Config::default(), Mode::Hold).await;
    let mut c = Client::new();
    r.exchange(&c.post(1, "/")).await;
    r.exchange(&window_update(1, 0x7fff_0000)).await;
    assert_eq!(
        r.exchange(&settings(&[(4, 0x7fff_ffff)])).await,
        vec![GoAway(FLOW_CONTROL)]
    );
}

#[tokio::test(start_paused = true)]
async fn the_connection_send_window_limits_all_streams_together() {
    let mut r = start(Config::default(), Mode::Body(40_000)).await;
    let mut c = Client::new();
    r.wire.send(&settings(&[(4, 100_000)])).await;
    r.exchange(&concat(&[c.get(1, "/"), c.get(3, "/")])).await;
    let sent: usize = data_frames(&r, 0).iter().map(|d| d.0).sum();
    assert_eq!(sent, 65_535);
    r.exchange(&window_update(0, 100_000)).await;
    let sent: usize = data_frames(&r, 0).iter().map(|d| d.0).sum();
    assert_eq!(sent, 80_000);
    assert_eq!(data_frames(&r, 0).iter().filter(|d| d.1).count(), 2);
}
