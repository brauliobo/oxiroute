//! Differential tests: every scenario is a raw-byte client script run against the `h2` crate's server and this
//! crate's server through the same in-memory harness; both must show the same class of reaction (GOAWAY code,
//! RST_STREAM code, response status). Rows with `differs` record an intentional difference and its reason.
mod support;
use std::future::poll_fn;

use bytes::Bytes;
use http::{HeaderMap, StatusCode};
use oxiroute_h2::server::{Config, Server};
use support::*;
use tokio::io::{DuplexStream, duplex};

use Reaction::{GoAway as G, Reset as R, Status as S};

/// Limits that map onto both servers' configuration.
#[derive(Clone, Copy, Default)]
struct Tweak {
    max_concurrent_streams: Option<u32>,
    max_header_list_size: Option<u32>,
    max_frame_size: Option<u32>,
    initial_window: Option<u32>,
    pending_accept_resets: Option<usize>,
    local_error_resets: Option<usize>,
    reset_stream_max: Option<usize>,
}

impl Tweak {
    fn ours(self) -> Config {
        let mut c = Config::default();
        if let Some(v) = self.max_concurrent_streams {
            c.max_concurrent_streams(v);
        }
        if let Some(v) = self.max_header_list_size {
            c.max_header_list_size(v);
        }
        if let Some(v) = self.max_frame_size {
            c.max_frame_size(v);
        }
        if let Some(v) = self.initial_window {
            c.initial_window_size(v);
        }
        if let Some(v) = self.pending_accept_resets {
            c.max_pending_accept_reset_streams(v);
        }
        if let Some(v) = self.local_error_resets {
            c.max_local_error_reset_streams(v);
        }
        if let Some(v) = self.reset_stream_max {
            c.reset_stream_max(v);
        }
        c
    }

    fn h2(self) -> h2::server::Builder {
        let mut b = h2::server::Builder::new();
        if let Some(v) = self.max_concurrent_streams {
            b.max_concurrent_streams(v);
        }
        if let Some(v) = self.max_header_list_size {
            b.max_header_list_size(v);
        }
        if let Some(v) = self.max_frame_size {
            b.max_frame_size(v);
        }
        if let Some(v) = self.initial_window {
            b.initial_window_size(v);
        }
        if let Some(v) = self.pending_accept_resets {
            b.max_pending_accept_reset_streams(v);
        }
        if let Some(v) = self.local_error_resets {
            b.max_local_error_reset_streams(Some(v));
        }
        if let Some(v) = self.reset_stream_max {
            b.max_concurrent_reset_streams(v);
        }
        b
    }
}

/// What the application does with an accepted request.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// Answers 200 with a two-octet body at once and drops the stream.
    Respond,
    /// Keeps the stream open and unread.
    Hold,
}

struct Scenario {
    name: &'static str,
    tweak: Tweak,
    mode: Mode,
    /// The script contains the preface and SETTINGS itself.
    raw_start: bool,
    /// Byte batches; the server settles after each.
    steps: Vec<Vec<u8>>,
    expect: Vec<Reaction>,
    differs: Option<(Vec<Reaction>, &'static str)>,
}

fn sc(name: &'static str, steps: Vec<Vec<u8>>) -> Scenario {
    Scenario {
        name,
        tweak: Tweak::default(),
        mode: Mode::Respond,
        raw_start: false,
        steps,
        expect: Vec::new(),
        differs: None,
    }
}

impl Scenario {
    fn tweak(mut self, t: Tweak) -> Self {
        self.tweak = t;
        self
    }

    fn hold(mut self) -> Self {
        self.mode = Mode::Hold;
        self
    }

    fn raw_start(mut self) -> Self {
        self.raw_start = true;
        self
    }

    fn expect(mut self, mut r: Vec<Reaction>) -> Self {
        r.sort();
        self.expect = r;
        self
    }

    fn differs(mut self, mut r: Vec<Reaction>, why: &'static str) -> Self {
        r.sort();
        self.differs = Some((r, why));
        self
    }
}

async fn serve_ours_mode(io: DuplexStream, config: Config, mode: Mode) {
    let Ok(mut server) = Server::handshake(io, config).await else {
        return;
    };
    let mut held = Vec::new();
    while let Some(Ok((_parts, mut stream))) = server.accept().await {
        if mode == Mode::Hold {
            held.push(stream);
            continue;
        }
        let mut headers = HeaderMap::new();
        headers.insert("content-length", "2".parse().unwrap());
        let _ = stream.send_response(StatusCode::OK, headers, false);
        if let Ok(2) = poll_fn(|cx| stream.poll_capacity(cx, 2)).await {
            let _ = stream.send_data(Bytes::from_static(b"ok"), true);
        }
    }
}

async fn serve_h2_mode(io: DuplexStream, builder: h2::server::Builder, mode: Mode) {
    let Ok(mut conn) = builder.handshake::<_, Bytes>(io).await else {
        return;
    };
    let mut held = Vec::new();
    while let Some(Ok((req, mut respond))) = conn.accept().await {
        if mode == Mode::Hold {
            held.push((req, respond));
            continue;
        }
        let response = http::Response::builder()
            .status(200)
            .header("content-length", "2")
            .body(())
            .unwrap();
        if let Ok(mut send) = respond.send_response(response, false) {
            let _ = send.send_data(Bytes::from_static(b"ok"), true);
        }
    }
}

fn run(sc: &Scenario, ours: bool) -> Vec<Reaction> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .start_paused(true)
        .build()
        .unwrap();
    rt.block_on(async {
        let (client, server) = duplex(1 << 20);
        if ours {
            tokio::spawn(serve_ours_mode(server, sc.tweak.ours(), sc.mode));
        } else {
            tokio::spawn(serve_h2_mode(server, sc.tweak.h2(), sc.mode));
        }
        let mut wire = Wire::new(client);
        if !sc.raw_start {
            wire.start().await;
            wire.settle().await;
        }
        for step in &sc.steps {
            use tokio::io::AsyncWriteExt;
            let _ = wire.io.write_all(step).await;
            let _ = wire.io.flush().await;
            wire.settle().await;
        }
        let frames = wire.frames.clone();
        wire.reactions(&frames)
    })
}

// ---- request builders ----

const BASE: [(&str, &str); 4] = [
    (":method", "GET"),
    (":scheme", "https"),
    (":authority", "example.test"),
    (":path", "/"),
];

/// HEADERS with END_HEADERS (+ END_STREAM when `end`) for exactly `fields`.
fn hdr(id: u32, fields: &[(&str, &str)], end: bool) -> Vec<u8> {
    let b = Client::new().block(fields);
    headers(id, &b, END_HEADERS | if end { END_STREAM } else { 0 })
}

fn get(id: u32) -> Vec<u8> {
    hdr(id, &BASE, true)
}

fn post(id: u32) -> Vec<u8> {
    hdr(id, &[(":method", "POST"), BASE[1], BASE[2], BASE[3]], false)
}

fn post_cl(id: u32, len: &str) -> Vec<u8> {
    hdr(
        id,
        &[
            (":method", "POST"),
            BASE[1],
            BASE[2],
            BASE[3],
            ("content-length", len),
        ],
        false,
    )
}

fn malformed(name: &'static str, fields: &[(&str, &str)]) -> Scenario {
    sc(name, vec![hdr(1, fields, true)]).expect(vec![R(1, 1)])
}

fn cat(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.concat()
}

fn rapid(n: u32) -> Vec<u8> {
    cat(&(0..n)
        .flat_map(|i| [get(2 * i + 1), rst(2 * i + 1, 8)])
        .collect::<Vec<_>>())
}

const HPACK: &str = "RFC 9113 4.3: HPACK failures are COMPRESSION_ERROR; h2 sends PROTOCOL_ERROR";
const SIZE: &str =
    "RFC 9113 4.2/6: a frame of the wrong size is FRAME_SIZE_ERROR; h2 sends PROTOCOL_ERROR";

fn post_hdr(id: u32, extra: &[(&str, &str)], end: bool) -> Vec<u8> {
    let mut f = vec![(":method", "POST"), BASE[1], BASE[2], BASE[3]];
    f.extend_from_slice(extra);
    hdr(id, &f, end)
}

fn get_with(extra: &[(&str, &str)]) -> Vec<u8> {
    let mut f = BASE.to_vec();
    f.extend_from_slice(extra);
    hdr(1, &f, true)
}

fn tiny(count: usize, len: usize) -> Vec<u8> {
    let mut f = vec![post(1)];
    f.extend((0..count).map(|_| data(1, &vec![1; len], 0)));
    cat(&f)
}

fn scenarios() -> Vec<Scenario> {
    let big = "x".repeat(1000);
    let small_list = Tweak {
        max_header_list_size: Some(512),
        ..Tweak::default()
    };
    let mut c = Client::new();
    let get_block = c.get_block("/");
    let mut v = vec![
        // ---- baseline ----
        sc("get_is_answered", vec![get(1)]).expect(vec![S(1, 200)]),
        sc("post_answered_early_cancels_body", vec![post(1)]).expect(vec![S(1, 200), R(1, 0)]),
        sc("two_gets_in_one_batch", vec![cat(&[get(1), get(3)])]).expect(vec![S(1, 200), S(3, 200)]),
        sc("unknown_settings_id_is_ignored", vec![settings(&[(0x99, 7)])]).expect(vec![]),
        sc("unknown_frame_type_is_ignored", vec![frame(0xfa, 0, 0, b"abc")]).expect(vec![]),
        sc("unknown_frame_type_on_stream_is_ignored", vec![frame(0xfa, 0, 5, b"abc")]).expect(vec![]),
        sc("goaway_from_client_is_tolerated", vec![goaway(0, 0)])
            .expect(vec![G(0)])
            .differs(vec![], "h2 answers the client's GOAWAY with its own GOAWAY(NO_ERROR); ours just ends the connection"),
        // ---- rapid reset ----
        sc("rapid_reset_under_pending_limit", vec![rapid(20)]).expect(vec![]),
        sc("rapid_reset_over_pending_limit", vec![rapid(21)]).expect(vec![G(11)]),
        sc("rapid_reset_small_limit", vec![rapid(4)])
            .tweak(Tweak { pending_accept_resets: Some(3), ..Tweak::default() })
            .expect(vec![G(11)]),
        sc("peer_resets_of_accepted_streams_are_counted", vec![
            cat(&(0..300).map(|i| post(2 * i + 1)).collect::<Vec<_>>()),
            cat(&(0..300).map(|i| rst(2 * i + 1, 8)).collect::<Vec<_>>()),
        ])
        .tweak(Tweak { max_concurrent_streams: Some(1000), ..Tweak::default() })
        .hold()
        .expect(vec![])
        .differs(vec![G(11)], "ours also limits peer resets of accepted streams (max_peer_reset_streams)"),
        // ---- local error reset budget ----
        sc("local_error_resets_within_budget", vec![cat(&[hdr(1, &BASE[..3], true), hdr(3, &BASE[..3], true)])])
            .tweak(Tweak { local_error_resets: Some(2), ..Tweak::default() })
            .expect(vec![R(1, 1), R(3, 1)]),
        sc(
            "local_error_resets_over_budget",
            vec![cat(&[hdr(1, &BASE[..3], true), hdr(3, &BASE[..3], true), hdr(5, &BASE[..3], true)])],
        )
        .tweak(Tweak { local_error_resets: Some(2), ..Tweak::default() })
        .expect(vec![G(11)])
        .differs(vec![G(11), R(1, 1), R(3, 1), R(5, 1)], "ours also flushes the RST_STREAMs queued before the GOAWAY"),
        // ---- CONTINUATION ----
        sc("continuation_completes_a_request", vec![cat(&[
            headers(1, &get_block[..3], END_STREAM),
            continuation(1, &get_block[3..], END_HEADERS),
        ])])
        .expect(vec![S(1, 200)]),
        sc("continuation_flood", vec![cat(&{
            let mut f = vec![headers(1, &get_block[..2], END_STREAM)];
            f.extend((0..30).map(|_| continuation(1, &[0], 0)));
            f
        })])
        .tweak(Tweak { max_header_list_size: Some(65_536), ..Tweak::default() })
        .expect(vec![G(11)]),
        sc("headers_followed_by_ping", vec![cat(&[headers(1, &get_block[..2], END_STREAM), ping([0; 8], false)])])
            .expect(vec![G(1)]),
        sc("headers_followed_by_other_stream_continuation", vec![cat(&[
            headers(1, &get_block[..2], END_STREAM),
            continuation(3, &get_block[2..], END_HEADERS),
        ])])
        .expect(vec![G(1)]),
        sc("stray_continuation", vec![continuation(1, &[0x82], END_HEADERS)]).expect(vec![G(1)]),
        sc("headers_followed_by_headers", vec![cat(&[headers(1, &get_block[..2], END_STREAM), get(3)])])
            .expect(vec![G(1)]),
        // ---- header list size ----
        sc("oversize_header_list_gets_431", vec![get_with(&[("x-big", &big)])])
            .tweak(small_list)
            .expect(vec![S(1, 431)]),
        sc("oversize_header_list_of_a_post_gets_431_and_reset", vec![post_hdr(1, &[("x-big", &big)], false)])
            .tweak(small_list)
            .expect(vec![S(1, 431), R(1, 1)]),
        sc("header_list_under_limit", vec![get_with(&[("x-big", &big)])])
            .tweak(Tweak { max_header_list_size: Some(2048), ..Tweak::default() })
            .expect(vec![S(1, 200)]),
        {
            // The first block puts a 300 octet value in the dynamic table (itself oversize); the second
            // references it ten times: a decoded list far beyond the limit from a tiny block.
            let mut c = Client::new();
            let v300 = "y".repeat(300);
            let pseudo = [BASE[0], BASE[1], BASE[2], BASE[3]];
            let first = c.block(&[pseudo[0], pseudo[1], pseudo[2], pseudo[3], ("x-bomb", &v300)]);
            let again = c.block(&[("x-bomb", &v300)]);
            let mut second = c.block(&pseudo);
            for _ in 0..10 {
                second.extend_from_slice(&again);
            }
            let both = cat(&[headers(1, &first, END_HEADERS | END_STREAM), headers(3, &second, END_HEADERS | END_STREAM)]);
            sc("hpack_bomb_way_over_the_list_limit", vec![both])
                .tweak(Tweak { max_header_list_size: Some(256), ..Tweak::default() })
                .expect(vec![G(11)])
                .differs(vec![S(1, 431), S(3, 431)], "ours has no 4x abuse multiplier: each oversize request is answered 431 and costs local error budget")
        },
        // ---- HPACK ----
        sc("hpack_index_zero", vec![headers(1, &[0x80], END_HEADERS | END_STREAM)]).expect(vec![G(1)]).differs(vec![G(9)], HPACK),
        sc("hpack_index_out_of_range", vec![headers(1, &[0xff, 0x7f], END_HEADERS | END_STREAM)])
            .expect(vec![G(1)])
            .differs(vec![G(9)], HPACK),
        sc("hpack_truncated_literal", vec![headers(1, &[0x40, 0x05, b'a'], END_HEADERS | END_STREAM)])
            .expect(vec![G(1)])
            .differs(vec![G(9)], HPACK),
        // ---- SETTINGS ----
        sc("settings_enable_push_two", vec![settings(&[(2, 2)])]).expect(vec![G(1)]),
        sc("settings_initial_window_too_large", vec![settings(&[(4, 1 << 31)])])
            .expect(vec![G(1)])
            .differs(vec![G(3)], "RFC 9113 6.5.2: INITIAL_WINDOW_SIZE above 2^31-1 is FLOW_CONTROL_ERROR; h2 sends PROTOCOL_ERROR"),
        sc("settings_max_frame_size_too_small", vec![settings(&[(5, 100)])]).expect(vec![G(1)]),
        sc("settings_max_frame_size_too_large", vec![settings(&[(5, 1 << 24)])]).expect(vec![G(1)]),
        sc("settings_length_not_multiple_of_six", vec![frame(SETTINGS, 0, 0, &[0; 5])]).expect(vec![G(1)]).differs(vec![G(6)], SIZE),
        sc("settings_ack_with_payload", vec![frame(SETTINGS, 1, 0, &[0; 6])]).expect(vec![G(1)]).differs(vec![G(6)], SIZE),
        sc("settings_ack_without_settings", vec![settings_ack()]).expect(vec![G(1)]),
        sc("settings_on_a_stream", vec![frame(SETTINGS, 0, 1, &[])]).expect(vec![G(1)]),
        sc("settings_valid_values", vec![settings(&[(1, 0), (3, 10), (4, 1000), (5, 20000), (6, 100)])]).expect(vec![]),
        // ---- PING ----
        sc("ping_wrong_length", vec![frame(PING, 0, 0, &[0; 7])]).expect(vec![G(1)]).differs(vec![G(6)], SIZE),
        sc("ping_on_a_stream", vec![frame(PING, 0, 1, &[0; 8])]).expect(vec![G(1)]),
        sc("ping_is_acknowledged", vec![ping([7; 8], false)]).expect(vec![]),
        // ---- DATA floods ----
        sc("empty_data_frames_under_the_limit", vec![{
            let mut f = vec![post(1)];
            f.extend((0..90).map(|_| data(1, &[], 0)));
            cat(&f)
        }])
        .hold()
        .expect(vec![]),
        sc("empty_data_frame_flood", vec![{
            let mut f = vec![post(1)];
            f.extend((0..150).map(|_| data(1, &[], 0)));
            cat(&f)
        }])
        .hold()
        .expect(vec![G(11)]),
        sc("tiny_data_frames_under_the_budget", vec![tiny(50, 1)]).hold().expect(vec![]),
        sc("tiny_data_frame_flood", vec![tiny(200, 1)]).hold().expect(vec![G(11)]),
        // ---- PRIORITY ----
        sc("priority_on_idle_stream", vec![priority(5, 3)]).expect(vec![]),
        sc("priority_wrong_length", vec![frame(2, 0, 1, &[0; 4])])
            .expect(vec![G(1)])
            .differs(vec![R(1, 6)], "RFC 9113 6.3: a PRIORITY of the wrong size is a stream error; h2 closes the connection"),
        sc("priority_self_dependency", vec![priority(1, 1)]).expect(vec![R(1, 1)]),
        sc("priority_on_stream_zero", vec![priority(0, 1)]).expect(vec![G(1)]),
        sc("priority_flood_is_counted", vec![cat(&(0..10_100).map(|_| priority(1, 3)).collect::<Vec<_>>())])
            .expect(vec![])
            .differs(vec![G(11)], "h2 ignores PRIORITY frames; ours counts them against max_ignored_frames"),
        // ---- concurrency and stream ids ----
        sc("stream_beyond_concurrency_limit_is_refused", vec![cat(&[post(1), post(3)])])
            .tweak(Tweak { max_concurrent_streams: Some(1), ..Tweak::default() })
            .hold()
            .expect(vec![R(3, 7)]),
        sc("even_stream_id_from_client", vec![hdr(2, &BASE, true)]).expect(vec![G(1)]),
        sc("decreasing_stream_id", vec![get(5), get(3)]).expect(vec![S(5, 200), G(1)]),
        sc("headers_on_a_finished_stream_id", vec![get(1), get(1)]).expect(vec![S(1, 200), G(1)]),
        sc("data_on_a_finished_stream", vec![get(1), data(1, b"x", 0)]).expect(vec![S(1, 200), R(1, 5)]),
        sc("data_on_an_idle_stream", vec![data(3, b"x", 0)]).expect(vec![G(1)]),
        sc("rst_stream_on_an_idle_stream", vec![rst(3, 8)]).expect(vec![G(1)]),
        sc("window_update_on_an_idle_stream", vec![window_update(3, 10)]).expect(vec![G(1)]),
        sc("rst_stream_on_a_finished_stream", vec![get(1), rst(1, 8)]).expect(vec![S(1, 200)]),
        sc("window_update_on_a_finished_stream", vec![get(1), window_update(1, 8)]).expect(vec![S(1, 200)]),
        sc("headers_on_half_closed_stream", vec![cat(&[post_hdr(1, &[], true), get(1)])])
            .hold()
            .expect(vec![G(1)])
            .differs(vec![R(1, 5)], "RFC 9113 5.1: HEADERS on a half-closed (remote) stream is a stream error STREAM_CLOSED; h2 closes the connection"),
        sc("data_after_a_peer_reset", vec![cat(&[post(1), rst(1, 8)]), data(1, b"x", 0)]).hold().expect(vec![R(1, 5)]),
        sc("headers_after_a_peer_reset", vec![cat(&[post(1), rst(1, 8)]), hdr(1, &[("x-t", "1")], true)])
            .hold()
            .expect(vec![G(1)]),
        sc("window_update_after_a_peer_reset", vec![cat(&[post(1), rst(1, 8)]), window_update(1, 5)])
            .hold()
            .expect(vec![]),
        // ---- frame sizes ----
        sc("frame_larger_than_max_frame_size", vec![frame(DATA, 0, 1, &vec![0; 16_385])]).expect(vec![G(6)]),
        sc("frame_at_a_raised_max_frame_size", vec![cat(&[post(1), frame(DATA, 0, 1, &vec![0; 20_000])])])
            .tweak(Tweak { max_frame_size: Some(32_768), initial_window: Some(1 << 20), ..Tweak::default() })
            .hold()
            .expect(vec![]),
        sc("rst_stream_wrong_length", vec![frame(RST_STREAM, 0, 1, &[0; 3])]).expect(vec![G(1)]).differs(vec![G(6)], SIZE),
        sc("window_update_wrong_length", vec![frame(WINDOW_UPDATE, 0, 0, &[0; 3])]).expect(vec![G(1)]).differs(vec![G(6)], SIZE),
        // ---- WINDOW_UPDATE and flow control ----
        sc("window_update_zero_on_connection", vec![window_update(0, 0)]).expect(vec![G(1)]),
        sc("window_update_zero_on_stream", vec![cat(&[post(1), window_update(1, 0)])])
            .hold()
            .expect(vec![G(1)])
            .differs(vec![R(1, 1)], "RFC 9113 6.9: a zero increment on a stream is a stream error; h2 closes the connection"),
        sc("window_update_overflow_on_connection", vec![window_update(0, 0x7fff_ffff)]).expect(vec![G(3)]),
        sc("window_update_overflow_on_stream", vec![cat(&[post(1), window_update(1, 0x7fff_ffff)])])
            .hold()
            .expect(vec![R(1, 3)]),
        sc("data_beyond_the_connection_window", vec![{
            let mut f = vec![post(1)];
            f.extend((0..5).map(|_| data(1, &[0; 16_000], 0)));
            cat(&f)
        }])
        .tweak(Tweak { initial_window: Some(1 << 20), ..Tweak::default() })
        .hold()
        .expect(vec![G(3)]),
        sc("data_beyond_the_stream_window", vec![cat(&[post(1), data(1, &[0; 1000], 0), data(1, &[0; 1000], 0)])])
            .tweak(Tweak { initial_window: Some(1500), ..Tweak::default() })
            .hold()
            .expect(vec![R(1, 3)]),
        // ---- content-length ----
        sc("content_length_longer_than_body", vec![cat(&[post_cl(1, "5"), data(1, b"abc", END_STREAM)])])
            .hold()
            .expect(vec![R(1, 1)]),
        sc("body_longer_than_content_length", vec![cat(&[post_cl(1, "2"), data(1, b"abc", END_STREAM)])])
            .hold()
            .expect(vec![R(1, 1)]),
        sc("content_length_matches_body", vec![cat(&[post_cl(1, "3"), data(1, b"abc", END_STREAM)])])
            .hold()
            .expect(vec![]),
        sc("content_length_with_end_stream_headers", vec![get_with(&[("content-length", "4")])]).expect(vec![R(1, 1)]),
        sc("content_length_not_a_number", vec![get_with(&[("content-length", "4x")])]).expect(vec![R(1, 1)]),
        sc("conflicting_content_lengths", vec![post_hdr(1, &[("content-length", "4"), ("content-length", "5")], false)])
            .expect(vec![R(1, 1)]),
        // ---- malformed requests ----
        malformed("missing_method", &BASE[1..]),
        malformed("missing_scheme", &[BASE[0], BASE[2], BASE[3]]),
        malformed("missing_path", &BASE[..3]),
        malformed("duplicate_path", &[BASE[0], BASE[1], BASE[2], BASE[3], (":path", "/b")]),
        malformed("pseudo_header_after_regular", &[BASE[0], BASE[1], BASE[2], ("accept", "*/*"), BASE[3]]),
        sc("uppercase_header_name", vec![get_with(&[("Accept", "*/*")])])
            .expect(vec![G(1)])
            .differs(vec![R(1, 1)], "RFC 9113 8.2.1: an uppercase field name is malformed (stream error); h2 closes the connection"),
        malformed("connection_header", &[BASE[0], BASE[1], BASE[2], BASE[3], ("connection", "close")]),
        malformed("transfer_encoding_header", &[BASE[0], BASE[1], BASE[2], BASE[3], ("transfer-encoding", "chunked")]),
        malformed("keep_alive_header", &[BASE[0], BASE[1], BASE[2], BASE[3], ("keep-alive", "5")]),
        malformed("proxy_connection_header", &[BASE[0], BASE[1], BASE[2], BASE[3], ("proxy-connection", "x")]),
        malformed("upgrade_header", &[BASE[0], BASE[1], BASE[2], BASE[3], ("upgrade", "websocket")]),
        malformed("te_other_than_trailers", &[BASE[0], BASE[1], BASE[2], BASE[3], ("te", "gzip")]),
        sc("te_trailers_is_allowed", vec![get_with(&[("te", "trailers")])]).expect(vec![S(1, 200)]),
        malformed("empty_path", &[BASE[0], BASE[1], BASE[2], (":path", "")]),
        malformed("status_pseudo_header_in_request", &[BASE[0], BASE[1], BASE[2], BASE[3], (":status", "200")]),
        sc("unknown_pseudo_header", vec![get_with(&[(":foo", "bar")])])
            .expect(vec![G(1)])
            .differs(vec![R(1, 1)], "RFC 9113 8.3: an unknown pseudo-header is malformed (stream error); h2 closes the connection"),
        sc("connect_with_authority_only", vec![hdr(1, &[(":method", "CONNECT"), (":authority", "example.test:443")], false)])
            .hold()
            .expect(vec![]),
        malformed("connect_with_scheme", &[(":method", "CONNECT"), (":scheme", "https"), (":authority", "e:443")]),
        malformed("connect_with_path", &[(":method", "CONNECT"), (":authority", "e:443"), (":path", "/")]),
        sc("connect_without_authority", vec![hdr(1, &[(":method", "CONNECT")], true)])
            .expect(vec![S(1, 200)])
            .differs(vec![R(1, 1)], "RFC 9113 8.5: CONNECT requires :authority; h2 accepts it"),
        // ---- padding ----
        {
            let mut p = vec![3];
            p.extend_from_slice(&get_block);
            p.extend_from_slice(&[0; 3]);
            sc("padded_headers", vec![headers(1, &p, END_HEADERS | END_STREAM | PADDED)]).expect(vec![S(1, 200)])
        },
        sc("padded_data", vec![cat(&[post(1), data(1, &[2, b'a', b'b', 0, 0], PADDED | END_STREAM)])])
            .hold()
            .expect(vec![]),
        sc("padded_data_with_pad_length_too_large", vec![cat(&[post(1), data(1, &[9, b'a'], PADDED)])])
            .hold()
            .expect(vec![G(1)]),
        // ---- stream state errors ----
        sc("trailers_without_end_stream", vec![cat(&[post(1), hdr(1, &[("x-t", "1")], false)])])
            .hold()
            .expect(vec![R(1, 1)]),
        sc("trailers_with_end_stream_are_accepted", vec![cat(&[post(1), hdr(1, &[("x-t", "1")], true)])])
            .hold()
            .expect(vec![]),
        sc("data_after_end_stream", vec![cat(&[post(1), data(1, b"a", END_STREAM), data(1, b"b", 0)])])
            .hold()
            .expect(vec![G(1)])
            .differs(vec![R(1, 5)], "RFC 9113 5.1: DATA on a half-closed (remote) stream is a stream error STREAM_CLOSED; h2 closes the connection"),
        sc("push_promise_from_client", vec![frame(5, END_HEADERS, 1, &[0, 0, 0, 2, 0x82])]).expect(vec![G(1)]),
        sc("headers_on_stream_zero", vec![frame(HEADERS, END_HEADERS, 0, &[0x82])]).expect(vec![G(1)]),
        sc("data_on_stream_zero", vec![frame(DATA, 0, 0, b"x")]).expect(vec![G(1)]),
        sc("rst_stream_on_stream_zero", vec![rst(0, 8)]).expect(vec![G(1)]),
        sc("goaway_on_a_stream", vec![frame(GOAWAY, 0, 1, &[0; 8])]).expect(vec![G(1)]),
        // ---- frames after a local reset ----
        sc("data_after_a_local_reset_is_ignored", vec![cat(&[post_cl(1, "2"), data(1, b"abc", 0), data(1, b"def", 0)])])
            .hold()
            .expect(vec![R(1, 1)]),
        sc(
            "data_after_a_forgotten_local_reset",
            vec![cat(&[post_cl(1, "2"), data(1, b"abc", 0), post_cl(3, "2"), data(3, b"abc", 0), data(1, b"x", 0)])],
        )
        .tweak(Tweak { reset_stream_max: Some(1), ..Tweak::default() })
        .hold()
        .expect(vec![R(1, 1), R(3, 1)])
        .differs(vec![R(1, 1), R(3, 1), R(1, 5)], "ours forgets a reset id once reset_stream_max newer resets exist (no timers); h2 expires by time"),
        // ---- first frame ----
        sc("first_frame_is_not_settings", vec![cat(&[PREFACE.to_vec(), get(1)])])
            .raw_start()
            .expect(vec![S(1, 200)])
            .differs(vec![G(1)], "RFC 9113 3.4: the first frame must be SETTINGS; h2 does not check"),
    ];
    v.shrink_to_fit();
    v
}

#[test]
fn both_servers_react_alike() {
    let print = std::env::var_os("DIFF_PRINT").is_some();
    let table = scenarios();
    assert!(table.len() >= 40, "{} scenarios", table.len());
    let mut failures = Vec::new();
    for s in &table {
        let theirs = run(s, false);
        let ours = run(s, true);
        if print {
            println!("{:<55} h2={theirs:?}\n{:<55} ours={ours:?}", s.name, "");
            continue;
        }
        let want_ours = s.differs.as_ref().map_or(&s.expect, |(r, _)| r);
        if theirs != s.expect || &ours != want_ours {
            failures.push(format!(
                "{}: expect {:?}; h2 {theirs:?}; ours {ours:?} (want {want_ours:?})",
                s.name, s.expect
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
