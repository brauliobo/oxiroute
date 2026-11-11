//! Raw-frame tests, part 3: request validation, padding and priority fields, dropped streams, shutdown.
#![allow(
    clippy::cast_possible_truncation,
    clippy::doc_markdown,
    clippy::too_many_lines
)]
#![allow(clippy::many_single_char_names)]
mod raw_rig;
mod support;

use oxiroute_h2::Reason;
use oxiroute_h2::server::Config;
use raw_rig::*;
use support::Reaction::{GoAway, Reset, Status};
use support::*;

// ---- request validation (RFC 9113 section 8) ----

fn request(c: &mut Client, fields: &[(&str, &str)], flags: u8) -> Vec<u8> {
    headers(1, &c.block(fields), flags | END_HEADERS)
}

const GET: [(&str, &str); 4] = [
    (":method", "GET"),
    (":scheme", "https"),
    (":authority", "example.test"),
    (":path", "/"),
];

fn with(extra: &[(&'static str, &'static str)]) -> Vec<(&'static str, &'static str)> {
    GET.iter().chain(extra).copied().collect()
}

#[tokio::test(start_paused = true)]
async fn malformed_requests_are_stream_errors() {
    let cases: Vec<(&str, Vec<(&str, &str)>)> = vec![
        ("no path", GET[..3].to_vec()),
        ("no method", GET[1..].to_vec()),
        ("no scheme", vec![GET[0], GET[2], GET[3]]),
        ("empty path", vec![GET[0], GET[1], GET[2], (":path", "")]),
        (
            "pseudo after regular",
            vec![GET[0], GET[1], GET[2], ("x-a", "b"), GET[3]],
        ),
        (
            "duplicate pseudo",
            vec![GET[0], GET[1], GET[2], GET[3], (":path", "/2")],
        ),
        (
            "unknown pseudo",
            with(&[])[..4]
                .iter()
                .copied()
                .chain([(":foo", "bar")])
                .collect(),
        ),
        (
            "response pseudo",
            vec![GET[0], GET[1], GET[2], GET[3], (":status", "200")],
        ),
        ("uppercase name", with(&[("X-Upper", "1")])),
        ("connection", with(&[("connection", "close")])),
        ("keep-alive", with(&[("keep-alive", "timeout=5")])),
        (
            "proxy-connection",
            with(&[("proxy-connection", "keep-alive")]),
        ),
        (
            "transfer-encoding",
            with(&[("transfer-encoding", "chunked")]),
        ),
        ("upgrade", with(&[("upgrade", "websocket")])),
        ("te other than trailers", with(&[("te", "gzip")])),
        ("bad content-length", with(&[("content-length", "abc")])),
        (
            "conflicting content-length",
            with(&[("content-length", "5"), ("content-length", "6")]),
        ),
        (
            "content-length with END_STREAM",
            with(&[("content-length", "5")]),
        ),
        (
            "bad method",
            vec![(":method", "G ET"), GET[1], GET[2], GET[3]],
        ),
    ];
    for (name, fields) in cases {
        let mut r = start(Config::default(), Mode::Respond).await;
        let mut c = Client::new();
        let out = r.exchange(&request(&mut c, &fields, END_STREAM)).await;
        assert_eq!(out, vec![Reset(1, PROTOCOL)], "{name}");
        // The connection and the HPACK state survive a stream error.
        assert_eq!(
            r.exchange(&c.get(3, "/")).await,
            vec![Status(3, 200)],
            "{name}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn te_trailers_and_repeated_content_length_are_accepted() {
    let mut r = start(Config::default(), Mode::Respond).await;
    let mut c = Client::new();
    let fields = with(&[
        ("te", "trailers"),
        ("content-length", "0"),
        ("content-length", "0"),
        ("cookie", "a=1"),
        ("cookie", "b=2"),
    ]);
    assert_eq!(
        r.exchange(&request(&mut c, &fields, END_STREAM)).await,
        vec![Status(1, 200)]
    );
}

#[tokio::test(start_paused = true)]
async fn plain_connect_is_accepted_and_connect_with_scheme_or_path_is_not() {
    let connect = [(":method", "CONNECT"), (":authority", "example.test:443")];
    let mut r = start(Config::default(), Mode::Respond).await;
    assert_eq!(
        r.exchange(&request(&mut Client::new(), &connect, END_STREAM))
            .await,
        vec![Status(1, 200)]
    );
    for extra in [(":scheme", "https"), (":path", "/")] {
        let mut r = start(Config::default(), Mode::Respond).await;
        let fields: Vec<_> = connect.iter().copied().chain([extra]).collect();
        assert_eq!(
            r.exchange(&request(&mut Client::new(), &fields, END_STREAM))
                .await,
            vec![Reset(1, PROTOCOL)]
        );
    }
    let mut r = start(Config::default(), Mode::Respond).await;
    let no_authority = [(":method", "CONNECT")];
    assert_eq!(
        r.exchange(&request(&mut Client::new(), &no_authority, END_STREAM))
            .await,
        vec![Reset(1, PROTOCOL)]
    );
}

#[tokio::test(start_paused = true)]
async fn extended_connect_is_not_supported() {
    let mut r = start(Config::default(), Mode::Respond).await;
    let fields = [
        (":method", "CONNECT"),
        (":protocol", "websocket"),
        (":scheme", "https"),
        (":authority", "example.test"),
        (":path", "/"),
    ];
    assert_eq!(
        r.exchange(&request(&mut Client::new(), &fields, END_STREAM))
            .await,
        vec![Reset(1, PROTOCOL)]
    );
}

#[tokio::test(start_paused = true)]
async fn content_length_must_match_the_body() {
    let post = |c: &mut Client, len: &'static str| {
        request(
            c,
            &with(&[("content-length", len)])[..]
                .iter()
                .map(|f| {
                    if f.1 == "GET" {
                        (":method", "POST")
                    } else {
                        *f
                    }
                })
                .collect::<Vec<_>>(),
            0,
        )
    };
    // more data than declared
    let mut r = start(Config::default(), Mode::Hold).await;
    let mut c = Client::new();
    assert_eq!(
        r.exchange(&concat(&[post(&mut c, "5"), data(1, &[0; 10], 0)]))
            .await,
        vec![Reset(1, PROTOCOL)]
    );
    // less data than declared
    let mut r = start(Config::default(), Mode::Hold).await;
    let mut c = Client::new();
    let wire = concat(&[post(&mut c, "5"), data(1, &[0; 3], END_STREAM)]);
    assert_eq!(r.exchange(&wire).await, vec![Reset(1, PROTOCOL)]);
    // END_STREAM through trailers while data is missing
    let mut r = start(Config::default(), Mode::Hold).await;
    let mut c = Client::new();
    let trailers = c.block(&[("x-t", "1")]);
    let wire = concat(&[
        post(&mut c, "5"),
        headers(1, &trailers, END_STREAM | END_HEADERS),
    ]);
    assert_eq!(r.exchange(&wire).await, vec![Reset(1, PROTOCOL)]);
    // exact
    let mut r = start(Config::default(), Mode::Respond).await;
    let mut c = Client::new();
    let wire = concat(&[
        post(&mut c, "5"),
        data(1, &[0; 2], 0),
        data(1, &[0; 3], END_STREAM),
    ]);
    assert_eq!(r.exchange(&wire).await, vec![Status(1, 200)]);
}

#[tokio::test(start_paused = true)]
async fn trailers_may_not_carry_pseudo_headers() {
    let mut r = start(Config::default(), Mode::Hold).await;
    let mut c = Client::new();
    let t = c.block(&[(":path", "/")]);
    let wire = concat(&[c.post(1, "/"), headers(1, &t, END_STREAM | END_HEADERS)]);
    assert_eq!(r.exchange(&wire).await, vec![Reset(1, PROTOCOL)]);
}

// ---- padding and priority fields ----

#[tokio::test(start_paused = true)]
async fn padded_headers_and_data_are_unpadded() {
    let mut r = start(Config::default(), Mode::Echo).await;
    let mut c = Client::new();
    let block = c.post_block("/");
    let padded = [&[3u8][..], &block, &[0, 0, 0]].concat();
    let body = [&[4u8][..], b"abc", &[0; 4]].concat();
    let out = r
        .exchange(&concat(&[
            headers(1, &padded, END_HEADERS | PADDED),
            data(1, &body, END_STREAM | PADDED),
        ]))
        .await;
    assert_eq!(out, vec![Status(1, 200)]);
    let echoed: Vec<u8> = r
        .wire
        .frames
        .iter()
        .filter(|f| f.kind == DATA)
        .flat_map(|f| f.payload.clone())
        .collect();
    assert_eq!(echoed, b"abc");
}

#[tokio::test(start_paused = true)]
async fn padding_longer_than_the_frame_is_a_protocol_error() {
    let mut r = start(Config::default(), Mode::Respond).await;
    let mut c = Client::new();
    let block = c.get_block("/");
    let bad = [&[200u8][..], &block].concat();
    assert_eq!(
        r.exchange(&headers(1, &bad, END_HEADERS | END_STREAM | PADDED))
            .await,
        vec![GoAway(PROTOCOL)]
    );
    let mut r = start(Config::default(), Mode::Hold).await;
    let wire = concat(&[Client::new().post(1, "/"), data(1, &[5, 1, 2], PADDED)]);
    assert_eq!(r.exchange(&wire).await, vec![GoAway(PROTOCOL)]);
}

#[tokio::test(start_paused = true)]
async fn padding_returns_its_flow_control_credit_at_once() {
    let mut r = start(
        cfg(|c| {
            c.max_empty_data_frames(1000);
        }),
        Mode::Hold,
    )
    .await;
    let mut c = Client::new();
    r.exchange(&c.post(1, "/")).await;
    // 400 frames of 256 octets (all padding) are 100 KB: more than either window unless credited back.
    let mut payload = vec![255u8];
    payload.extend([0; 255]);
    let out = r
        .exchange(
            &(0..400)
                .flat_map(|_| data(1, &payload, PADDED))
                .collect::<Vec<u8>>(),
        )
        .await;
    assert_eq!(out, vec![]);
    let credited: u32 = r
        .wire
        .frames
        .iter()
        .filter(|f| f.kind == WINDOW_UPDATE && f.stream == 0)
        .map(|f| u32::from_be_bytes(f.payload[..4].try_into().unwrap()))
        .sum();
    assert!(credited >= 100_000 - 32_768, "credited {credited}");
}

#[tokio::test(start_paused = true)]
async fn headers_with_a_priority_field_are_decoded() {
    let mut r = start(Config::default(), Mode::Respond).await;
    let mut c = Client::new();
    let block = c.get_block("/");
    let with_priority = [&[0, 0, 0, 3, 16][..], &block].concat();
    let out = r
        .exchange(&headers(
            1,
            &with_priority,
            END_HEADERS | END_STREAM | PRIORITY_FLAG,
        ))
        .await;
    assert_eq!(out, vec![Status(1, 200)]);
    // A stream depending on itself is a stream error (RFC 9113 5.3.1); the block still updates HPACK.
    let own = [&[0, 0, 0, 3, 16][..], &c.get_block("/")].concat();
    let out = r
        .exchange(&headers(3, &own, END_HEADERS | END_STREAM | PRIORITY_FLAG))
        .await;
    assert_eq!(out, vec![Reset(3, PROTOCOL)]);
    assert_eq!(r.exchange(&c.get(5, "/")).await, vec![Status(5, 200)]);
}

// ---- dropping a stream ----

#[tokio::test(start_paused = true)]
async fn dropping_a_stream_without_a_response_cancels_it() {
    let mut r = start(Config::default(), Mode::Drop).await;
    let mut c = Client::new();
    assert_eq!(r.exchange(&c.get(1, "/")).await, vec![Reset(1, 8)]);
}

// ---- shutdown ----

fn goaway_last_ids(r: &Rig) -> Vec<(u32, u32)> {
    r.wire
        .frames
        .iter()
        .filter(|f| f.kind == GOAWAY)
        .map(|f| {
            (
                u32::from_be_bytes(f.payload[..4].try_into().unwrap()),
                goaway_code(f),
            )
        })
        .collect()
}

#[tokio::test(start_paused = true)]
async fn graceful_shutdown_announces_twice_and_still_serves_streams_opened_meanwhile() {
    let mut r = start(Config::default(), Mode::Respond).await;
    let mut c = Client::new();
    assert_eq!(r.exchange(&c.get(1, "/")).await, vec![Status(1, 200)]);
    r.ctl.send(Cmd::Graceful).unwrap();
    r.settle().await;
    assert_eq!(goaway_last_ids(&r), vec![(0x7fff_ffff, 0)]);
    let probe = r
        .wire
        .frames
        .iter()
        .find(|f| f.kind == PING)
        .expect("shutdown ping");
    assert_eq!(probe.flags, 0);
    let payload: [u8; 8] = probe.payload.clone().try_into().unwrap();
    // The client had already sent stream 3 when it saw the GOAWAY.
    let out = r
        .exchange(&concat(&[c.get(3, "/"), ping(payload, true)]))
        .await;
    assert_eq!(out, vec![GoAway(0), Status(3, 200)]);
    assert_eq!(goaway_last_ids(&r), vec![(0x7fff_ffff, 0), (3, 0)]);
    assert!(
        r.task.await.unwrap().is_ok(),
        "accept ends once the last stream is done"
    );
}

#[tokio::test(start_paused = true)]
async fn graceful_shutdown_waits_for_open_streams_and_ignores_later_ones() {
    let mut r = start(Config::default(), Mode::Hold).await;
    let mut c = Client::new();
    r.exchange(&c.post(1, "/")).await;
    r.ctl.send(Cmd::Graceful).unwrap();
    r.settle().await;
    let payload: [u8; 8] = r
        .wire
        .frames
        .iter()
        .find(|f| f.kind == PING)
        .unwrap()
        .payload
        .clone()
        .try_into()
        .unwrap();
    r.exchange(&ping(payload, true)).await;
    assert_eq!(goaway_last_ids(&r).last(), Some(&(1, 0)));
    // Past the final GOAWAY: ignored, neither answered nor an error.
    assert_eq!(
        r.exchange(&concat(&[c.post(3, "/"), data(3, b"x", 0)]))
            .await,
        vec![]
    );
    assert!(!r.task.is_finished(), "stream 1 is still open");
    r.exchange(&rst(1, 8)).await;
    assert!(r.task.await.unwrap().is_ok());
}

#[tokio::test(start_paused = true)]
async fn abrupt_shutdown_sends_goaway_with_the_reason() {
    let mut r = start(Config::default(), Mode::Hold).await;
    let mut c = Client::new();
    r.exchange(&c.post(1, "/")).await;
    r.ctl.send(Cmd::Abrupt(Reason::ENHANCE_YOUR_CALM)).unwrap();
    assert_eq!(r.settle().await, vec![GoAway(CALM)]);
    assert_eq!(goaway_last_ids(&r), vec![(1, CALM)]);
    assert!(r.task.await.unwrap().is_ok());
}
