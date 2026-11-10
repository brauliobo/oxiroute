//! Raw-frame tests: drive the server with hand-built frames and assert the observable reaction (GOAWAY code,
//! RST_STREAM code or response status). Paused time makes `Wire::settle` a deterministic quiescence barrier.
mod support;
use std::future::poll_fn;
use std::time::Duration;

use bytes::Bytes;
use oxiroute_h2::server::{Config, Server};
use oxiroute_h2::{Error, Reason};
use support::Reaction::{GoAway, Reset, Status};
use support::*;
use tokio::io::duplex;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

const PROTOCOL: u32 = 1;
const FLOW_CONTROL: u32 = 3;
const SETTINGS_TIMEOUT: u32 = 4;
const STREAM_CLOSED: u32 = 5;
const FRAME_SIZE: u32 = 6;
const REFUSED: u32 = 7;
const COMPRESSION: u32 = 9;
const CALM: u32 = 11;

/// What the application does with an accepted request.
#[derive(Clone, Copy)]
enum Mode {
    /// 200 with a two-octet body, inline.
    Respond,
    /// Keeps the stream open without answering.
    Hold,
    /// Reads the body and sends it back.
    Echo,
    /// 200 with a body of this many octets.
    Body(usize),
}

enum Cmd {
    Graceful,
    Abrupt(Reason),
}

struct Rig {
    wire: Wire,
    task: JoinHandle<Result<(), Error>>,
    ctl: mpsc::UnboundedSender<Cmd>,
}

async fn serve(
    io: tokio::io::DuplexStream,
    config: Config,
    mode: Mode,
    mut rx: mpsc::UnboundedReceiver<Cmd>,
) -> Result<(), Error> {
    let mut server = Server::handshake(io, config).await?;
    let mut held = Vec::new();
    loop {
        tokio::select! {
            r = server.accept() => {
                let Some(r) = r else { return Ok(()) };
                let (_parts, mut stream) = r?;
                match mode {
                    Mode::Respond => {
                        let (status, headers) = response(200, &[]);
                        stream.send_response(status, headers, false)?;
                        write_body(&mut stream, Bytes::from_static(b"ok"), true).await?;
                    }
                    Mode::Hold => held.push(stream),
                    Mode::Echo => {
                        tokio::spawn(async move {
                            let Ok(body) = read_body(&mut stream).await else { return };
                            let (status, headers) = response(200, &[]);
                            stream.send_response(status, headers, false).unwrap();
                            write_body(&mut stream, Bytes::from(body), true).await.unwrap();
                        });
                    }
                    Mode::Body(n) => {
                        tokio::spawn(async move {
                            let (status, headers) = response(200, &[]);
                            stream.send_response(status, headers, false).unwrap();
                            let body: Vec<u8> = (0..n).map(|i| i as u8).collect();
                            write_body(&mut stream, Bytes::from(body), true).await.unwrap();
                        });
                    }
                }
            }
            Some(cmd) = rx.recv() => match cmd {
                Cmd::Graceful => server.graceful_shutdown(),
                Cmd::Abrupt(reason) => server.abrupt_shutdown(reason),
            },
        }
    }
}

/// Server running `mode` behind a pipe of `buf` octets per direction; the client has not sent anything yet.
fn rig(config: Config, mode: Mode, buf: usize) -> Rig {
    let (client, server) = duplex(buf);
    let (ctl, rx) = mpsc::unbounded_channel();
    Rig {
        wire: Wire::new(client),
        task: tokio::spawn(serve(server, config, mode, rx)),
        ctl,
    }
}

/// Server with a completed preface and SETTINGS exchange.
async fn start(config: Config, mode: Mode) -> Rig {
    let mut r = rig(config, mode, 1 << 20);
    r.wire.start().await;
    r
}

impl Rig {
    /// Sends `bytes`, waits for quiescence and returns the reactions.
    async fn exchange(&mut self, bytes: &[u8]) -> Vec<Reaction> {
        self.wire.send(bytes).await;
        self.settle().await
    }

    async fn settle(&mut self) -> Vec<Reaction> {
        let frames = self.wire.settle().await;
        self.wire.reactions(&frames)
    }
}

fn cfg(f: impl FnOnce(&mut Config)) -> Config {
    let mut c = Config::default();
    f(&mut c);
    c
}

fn concat(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.concat()
}

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
async fn a_request_over_the_header_list_limit_is_answered_431_and_reset() {
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
    assert_eq!(out, vec![Reset(1, PROTOCOL), Status(1, 431)]);
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
    assert_eq!(
        r.exchange(&bomb).await,
        vec![Reset(3, PROTOCOL), Status(3, 431)]
    );
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
