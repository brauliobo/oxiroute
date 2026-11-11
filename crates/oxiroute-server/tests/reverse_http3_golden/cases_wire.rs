//! Protocol-level behaviour driven by a raw quinn client: frame order, control and QPACK streams, resets.
//!
//! RFC 9114 and 9204 mandate most of the codes recorded here. Where the recorded `h3` crate path is lenient,
//! the case is tolerant and also accepts the mandated connection error.

use std::time::Duration;

use serde_json::{Value, json};
use tokio::time::sleep;

use crate::{
    client,
    context::{Context, host},
    golden::Tolerance,
    raw::{
        self, CANCEL_PUSH, DATA, GOAWAY, HEADERS, MAX_PUSH_ID, PUSH_PROMISE, Raw, SETTINGS, frame,
        headers,
    },
};

pub fn ok_request(host: &str) -> Vec<u8> {
    headers(&raw::base(host, "/fixed/ok"))
}

/// Records the answer to the first exchange, then, if the connection survived, one ordinary request.
pub async fn conclude(
    c: &mut Context,
    id: &str,
    request: &str,
    raw: Raw,
    outcome: Value,
    tolerance: Option<Tolerance>,
) {
    let host = host(id);
    sleep(Duration::from_millis(50)).await;
    let mut steps = vec![c.observe(request, outcome).await];
    steps[0]["connection_after"] = client::connection_state(&raw.conn);
    if raw.conn.close_reason().is_none() {
        let answer = raw.exchange(&ok_request(&host)).await;
        let mut step = c.observe("follow-up GET /fixed/ok", answer).await;
        step["connection_after"] = client::connection_state(&raw.conn);
        steps.push(step);
    }
    raw.finish();
    c.record(id, vec![host], steps, tolerance);
}

/// `bytes` on a new request stream (finished), then the follow-up request.
pub async fn request_case(
    c: &mut Context,
    id: &str,
    description: &str,
    bytes: &[u8],
    tolerance: Option<Tolerance>,
) {
    let raw = Raw::open(&c.endpoint, c.addresses.listener).await;
    let outcome = raw.exchange(bytes).await;
    conclude(c, id, description, raw, outcome, tolerance).await;
}

pub fn post(host: &str, path: &str, content_length: usize) -> Vec<u8> {
    headers(&raw::with(
        raw::request("POST", host, path),
        &[("content-length", &content_length.to_string())],
    ))
}

fn trailers() -> Vec<u8> {
    headers(&vec![("x-trailer".to_owned(), "t".to_owned())])
}

fn data(bytes: &[u8]) -> Vec<u8> {
    frame(DATA, bytes)
}

pub async fn run(c: &mut Context) {
    frames(c).await;
    framing_that_is_accepted(c).await;
    control_stream(c).await;
    unidirectional_streams(c).await;
    qpack(c).await;
    stream_lifecycle(c).await;
}

async fn frames(c: &mut Context) {
    let id = "wire/first-frame-is-empty-data";
    request_case(c, id, "empty DATA frame first", &data(b""), None).await;
    let id = "wire/data-before-headers";
    request_case(
        c,
        id,
        "DATA then HEADERS",
        &[data(b"abc"), ok_request(&host(id))].concat(),
        None,
    )
    .await;
    let id = "wire/settings-on-a-request-stream";
    request_case(
        c,
        id,
        "SETTINGS first",
        &[frame(SETTINGS, &[]), ok_request(&host(id))].concat(),
        None,
    )
    .await;
    let id = "wire/goaway-on-a-request-stream";
    request_case(
        c,
        id,
        "GOAWAY first",
        &[frame(GOAWAY, &[0]), ok_request(&host(id))].concat(),
        None,
    )
    .await;
    request_case(
        c,
        "wire/cancel-push-on-a-request-stream",
        "CANCEL_PUSH first",
        &frame(CANCEL_PUSH, &[0]),
        None,
    )
    .await;
    request_case(
        c,
        "wire/push-promise-from-a-client",
        "PUSH_PROMISE first",
        &frame(PUSH_PROMISE, &[0, 0, 0]),
        None,
    )
    .await;
    request_case(
        c,
        "wire/max-push-id-on-a-request-stream",
        "MAX_PUSH_ID first",
        &frame(MAX_PUSH_ID, &[0]),
        None,
    )
    .await;
    for kind in [0x2, 0x6, 0x8, 0x9] {
        let id = format!("wire/reserved-http2-frame-type-{kind:#x}");
        request_case(
            c,
            &id,
            "frame type of HTTP/2 with no HTTP/3 meaning",
            &frame(kind, &[0]),
            None,
        )
        .await;
    }
    let id = "wire/headers-after-trailers";
    let bytes = [
        post(&host(id), "/fixed/ok", 1),
        data(b"x"),
        trailers(),
        trailers(),
    ]
    .concat();
    request_case(c, id, "HEADERS DATA HEADERS HEADERS", &bytes, None).await;
    let id = "wire/data-after-trailers";
    let bytes = [
        post(&host(id), "/fixed/ok", 1),
        data(b"x"),
        trailers(),
        data(b"y"),
    ]
    .concat();
    request_case(c, id, "HEADERS DATA HEADERS DATA", &bytes, None).await;
    let id = "wire/truncated-frame-at-the-end-of-the-stream";
    let bytes = [ok_request(&host(id)), vec![DATA as u8, 10, 1, 2]].concat();
    request_case(c, id, "HEADERS then a DATA frame cut short", &bytes, None).await;
}

async fn framing_that_is_accepted(c: &mut Context) {
    let id = "wire/baseline-request";
    request_case(c, id, "HEADERS", &ok_request(&host(id)), None).await;
    let id = "wire/unknown-frames-before-headers";
    let unknown = [
        frame(0x21, &[7; 300]),
        frame(0x40, b"later"),
        frame(0x1f * 5 + 0x21, &[]),
        ok_request(&host(id)),
    ]
    .concat();
    request_case(c, id, "unknown frame types then HEADERS", &unknown, None).await;
    let id = "wire/unknown-frame-between-data-frames";
    let split = [
        post(&host(id), "/p/echo", 10),
        data(b"hello"),
        frame(0x21, b"skip me"),
        data(b"world"),
    ]
    .concat();
    request_case(c, id, "HEADERS DATA unknown DATA", &split, None).await;
    let id = "wire/body-in-many-data-frames";
    let many = [
        post(&host(id), "/p/echo", 5000),
        data(&[1; 1000]),
        data(&[2; 1000]),
        data(&[3; 3000]),
    ]
    .concat();
    request_case(c, id, "three DATA frames of 5000 bytes", &many, None).await;
    let id = "wire/empty-data-frame-in-a-body";
    let empty = [
        post(&host(id), "/p/echo", 3),
        data(b"ab"),
        data(b""),
        data(b"c"),
    ]
    .concat();
    request_case(
        c,
        id,
        "DATA of 2 bytes, an empty DATA, DATA of 1 byte",
        &empty,
        None,
    )
    .await;
    let id = "wire/trailers-without-data";
    let bytes = [headers(&raw::base(&host(id), "/p/echo")), trailers()].concat();
    request_case(c, id, "HEADERS HEADERS(trailers)", &bytes, None).await;
    let id = "wire/unknown-frame-after-trailers";
    let after = [
        post(&host(id), "/fixed/ok", 1),
        data(b"x"),
        trailers(),
        frame(0x21, b"late"),
    ]
    .concat();
    request_case(c, id, "HEADERS DATA HEADERS unknown", &after, None).await;
}

async fn control_stream(c: &mut Context) {
    let (endpoint, address) = (c.endpoint.clone(), c.addresses.listener);
    let two = |first: Vec<u8>, second: Vec<u8>| [frame(SETTINGS, &[]), first, second].concat();
    let cases: Vec<(&str, &str, Vec<u8>, Option<Tolerance>)> = vec![
        (
            "wire/control-first-frame-is-goaway",
            "GOAWAY first on the control stream",
            frame(GOAWAY, &[0]),
            None,
        ),
        (
            "wire/control-settings-twice",
            "SETTINGS, SETTINGS",
            two(frame(SETTINGS, &[]), vec![]),
            None,
        ),
        (
            "wire/settings-reserved-http2-identifier",
            "SETTINGS with identifier 0x2",
            frame(SETTINGS, &[0x02, 0x01]),
            None,
        ),
        (
            "wire/settings-duplicate-identifier",
            "SETTINGS with identifier 0x21 twice",
            frame(SETTINGS, &[0x21, 0x01, 0x21, 0x02]),
            None,
        ),
        (
            "wire/goaway-id-increases",
            "SETTINGS GOAWAY 8 GOAWAY 12",
            two(frame(GOAWAY, &[8]), frame(GOAWAY, &[12])),
            None,
        ),
        (
            "wire/max-push-id-decreases",
            "SETTINGS MAX_PUSH_ID 8 then 4",
            two(frame(MAX_PUSH_ID, &[8]), frame(MAX_PUSH_ID, &[4])),
            None,
        ),
        (
            "wire/cancel-push-without-a-push-id-grant",
            "SETTINGS CANCEL_PUSH 0",
            two(frame(CANCEL_PUSH, &[0]), vec![]),
            None,
        ),
        (
            "wire/control-data-frame",
            "SETTINGS DATA on the control stream",
            two(data(b"x"), vec![]),
            None,
        ),
        (
            "wire/control-headers-frame",
            "SETTINGS HEADERS on the control stream",
            two(frame(HEADERS, b"x"), vec![]),
            None,
        ),
        (
            "wire/control-unknown-frames-are-ignored",
            "SETTINGS, unknown frame",
            two(frame(0x21, &[0; 64]), vec![]),
            None,
        ),
        (
            "wire/settings-unknown-identifier-is-ignored",
            "SETTINGS with identifier 0x21",
            frame(SETTINGS, &[0x21, 0x01]),
            None,
        ),
    ];
    for (id, description, bytes, tolerance) in cases {
        let raw = Raw::open_raw_control(&endpoint, address, &bytes).await;
        let outcome = raw.closed(raw::CLOSE_WAIT).await;
        conclude(c, id, description, raw, outcome, tolerance).await;
    }
    let id = "wire/control-stream-without-settings";
    let raw = Raw::open_raw_control(&endpoint, address, &[]).await;
    let outcome = raw.exchange(&ok_request(&host(id))).await;
    conclude(
        c,
        id,
        "request while SETTINGS is missing",
        raw,
        outcome,
        None,
    )
    .await;
    let id = "wire/no-control-stream";
    let raw = Raw::without_control(&endpoint, address).await;
    let outcome = raw.exchange(&ok_request(&host(id))).await;
    conclude(
        c,
        id,
        "request without a control stream",
        raw,
        outcome,
        None,
    )
    .await;
    control_stream_endings(c).await;
}

async fn control_stream_endings(c: &mut Context) {
    let (endpoint, address) = (c.endpoint.clone(), c.addresses.listener);
    let mut raw = Raw::open(&endpoint, address).await;
    raw.control
        .as_mut()
        .expect("control stream")
        .finish()
        .expect("finish control stream");
    let outcome = raw.closed(raw::CLOSE_WAIT).await;
    conclude(
        c,
        "wire/control-stream-is-finished",
        "FIN on the control stream",
        raw,
        outcome,
        None,
    )
    .await;
    let mut raw = Raw::open(&endpoint, address).await;
    raw.control
        .as_mut()
        .expect("control stream")
        .reset(0u32.into())
        .expect("reset control stream");
    let outcome = raw.closed(raw::CLOSE_WAIT).await;
    conclude(
        c,
        "wire/control-stream-is-reset",
        "RESET_STREAM on the control stream",
        raw,
        outcome,
        None,
    )
    .await;
}

async fn unidirectional_streams(c: &mut Context) {
    let (endpoint, address) = (c.endpoint.clone(), c.addresses.listener);
    let cases: Vec<(&str, &str, u64, Vec<u8>, usize, Option<Tolerance>)> = vec![
        (
            "wire/push-stream-from-a-client",
            "stream type 0x01",
            0x01,
            vec![0],
            1,
            None,
        ),
        (
            "wire/second-control-stream",
            "second stream type 0x00",
            0x00,
            frame(SETTINGS, &[]),
            1,
            None,
        ),
        (
            "wire/second-qpack-encoder-stream",
            "two streams type 0x02",
            0x02,
            Vec::new(),
            2,
            None,
        ),
        (
            "wire/second-qpack-decoder-stream",
            "two streams type 0x03",
            0x03,
            Vec::new(),
            2,
            None,
        ),
        (
            "wire/qpack-section-acknowledgment-for-nothing",
            "decoder instruction for no section",
            0x03,
            vec![0x84],
            1,
            None,
        ),
        (
            "wire/qpack-insert-count-increment-for-nothing",
            "decoder instruction without inserts",
            0x03,
            vec![0x01],
            1,
            None,
        ),
        (
            "wire/qpack-set-capacity-above-the-limit",
            "encoder instruction: capacity 220",
            0x02,
            vec![0x3f, 0xbd, 0x01],
            1,
            None,
        ),
    ];
    for (id, description, kind, bytes, count, tolerance) in cases {
        let raw = Raw::open(&endpoint, address).await;
        let mut keep = Vec::new();
        for _ in 0..count {
            keep.push(raw.uni(kind, &bytes).await);
        }
        let outcome = raw.closed(raw::CLOSE_WAIT).await;
        conclude(c, id, description, raw, outcome, tolerance).await;
        drop(keep);
    }
    let id = "wire/unknown-unidirectional-stream-types";
    let raw = Raw::open(&endpoint, address).await;
    let streams = [
        raw.uni(0x1f * 3 + 0x21, &[1, 2, 3]).await,
        raw.uni(0x40, &[9; 100]).await,
        raw.uni(0x3f * 7 + 0x21, &[]).await,
    ];
    let outcome = raw.exchange(&ok_request(&host(id))).await;
    conclude(
        c,
        id,
        "reserved and unknown stream types, then a request",
        raw,
        outcome,
        None,
    )
    .await;
    drop(streams);
}

/// A request whose field section refers to entry 0 of the dynamic table.
fn dynamic_reference_request(host: &str, required_insert_count: u8) -> Vec<u8> {
    let mut block = vec![
        required_insert_count,
        0x00,
        0xd1,
        0xd7,
        0x50,
        host.len() as u8,
    ];
    block.extend_from_slice(host.as_bytes());
    block.extend_from_slice(&[0x51, 9]);
    block.extend_from_slice(b"/fixed/ok");
    block.push(0x80);
    frame(HEADERS, &block)
}

async fn qpack(c: &mut Context) {
    let (endpoint, address) = (c.endpoint.clone(), c.addresses.listener);
    let id = "wire/qpack-dynamic-table-use";
    let raw = Raw::open(&endpoint, address).await;
    let mut insert = vec![0x3f, 0xbd, 0x01, 0x45];
    insert.extend_from_slice(b"x-dyn");
    insert.push(0x03);
    insert.extend_from_slice(b"dyn");
    let encoder = raw.uni(0x02, &insert).await;
    sleep(Duration::from_millis(100)).await;
    let outcome = raw.exchange(&dynamic_reference_request(&host(id), 2)).await;
    conclude(
        c,
        id,
        "table capacity 220, one insert, one reference",
        raw,
        outcome,
        None,
    )
    .await;
    drop(encoder);
    let id = "wire/qpack-reference-without-an-insert";
    let raw = Raw::open(&endpoint, address).await;
    let outcome = raw.exchange(&dynamic_reference_request(&host(id), 2)).await;
    conclude(
        c,
        id,
        "section needing an insert that never arrived",
        raw,
        outcome,
        None,
    )
    .await;
    let id = "wire/qpack-invalid-huffman";
    let raw = Raw::open(&endpoint, address).await;
    let block = [&[0u8, 0, 0x50, 0x84][..], &[0xff; 4]].concat();
    let outcome = raw.exchange(&frame(HEADERS, &block)).await;
    conclude(
        c,
        id,
        "Huffman string with invalid padding",
        raw,
        outcome,
        None,
    )
    .await;
}

async fn stream_lifecycle(c: &mut Context) {
    let (endpoint, address) = (c.endpoint.clone(), c.addresses.listener);
    let id = "wire/client-resets-the-stream-mid-body";
    let raw = Raw::open(&endpoint, address).await;
    let (mut send, _recv) = raw
        .request(&[post(&host(id), "/p/echo", 100), data(&[7; 10])].concat())
        .await;
    sleep(Duration::from_millis(100)).await;
    send.reset(0x10cu32.into()).expect("reset request stream");
    let tolerance = Tolerance::new(
        "whether and how an abandoned request is logged depends on when the server notices the reset",
    );
    conclude(
        c,
        id,
        "POST with 10 of 100 bytes, then RESET_STREAM",
        raw,
        json!({ "reset_sent": true }),
        Some(tolerance),
    )
    .await;
    let id = "wire/client-stops-the-response";
    let raw = Raw::open(&endpoint, address).await;
    let (mut send, mut recv) = raw
        .request(&headers(&raw::base(&host(id), "/fixed/large")))
        .await;
    drop(send.finish());
    recv.stop(0x10cu32.into()).expect("stop sending");
    let tolerance = Tolerance::new(
        "whether and how an abandoned response is logged depends on when the server notices the stop",
    );
    conclude(
        c,
        id,
        "STOP_SENDING on a response",
        raw,
        json!({ "stop_sent": true }),
        Some(tolerance),
    )
    .await;
    let id = "wire/stream-finished-without-a-request";
    let raw = Raw::open(&endpoint, address).await;
    let outcome = raw.exchange(&[]).await;
    conclude(c, id, "FIN with no frames", raw, outcome, None).await;
}
