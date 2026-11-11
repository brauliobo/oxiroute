//! Request validity and bounds: body limits, header section limits, malformed requests, stream and connection limits.

use std::time::Duration;

use serde_json::{Value, json};
use tokio::time::timeout;

use crate::{
    cases_wire::{conclude, request_case},
    client::{Body, Req},
    config::{LISTENER_CONNECTIONS, ROUTE_BODY_LIMIT},
    context::{Context, host},
    golden::Tolerance,
    raw::{self, DATA, Raw, frame},
    stats,
};

const SECTION_LIMIT: usize = 16 * 1024;
const BLOCKED: Duration = Duration::from_millis(400);

pub async fn run(c: &mut Context) {
    unsendable_response(c).await;
    body_limits(c).await;
    content_length(c).await;
    invalid_paths(c).await;
    header_sections(c).await;
    malformed_requests(c).await;
    stream_limits(c).await;
    connection_limit(c).await;
}

fn bytes(length: usize) -> Body {
    Body::Bytes(vec![b'z'; length])
}

async fn body_limits(c: &mut Context) {
    let limited = |method: &'static str| Req::new(method, "/limit/x");
    c.run(
        "body/at-the-route-limit",
        vec![limited("POST").body(bytes(ROUTE_BODY_LIMIT as usize))],
    )
    .await;
    c.run(
        "body/over-the-route-limit",
        vec![limited("POST").body(bytes(ROUTE_BODY_LIMIT as usize + 1))],
    )
    .await;
    let id = "body/over-the-limit-streamed";
    let streamed = [
        raw::headers(&raw::request("POST", &host(id), "/limit/x")),
        frame(DATA, &[b'a'; 5]),
        frame(DATA, &[b'b'; 5]),
    ]
    .concat();
    request_case(c, id, "streamed body of 10 bytes", &streamed, None).await;
    c.run(
        "body/within-the-limit-in-chunks",
        vec![limited("POST").body(Body::Chunks(vec![vec![b'a'; 4], vec![b'b'; 4]]))],
    )
    .await;
    c.run(
        "body/over-the-limit-by-content-length",
        vec![limited("POST").header("content-length", "9")],
    )
    .await;
    c.run(
        "body/over-the-limit-by-content-length-with-the-body",
        vec![limited("POST").header("content-length", "9").body(bytes(9))],
    )
    .await;
    c.run(
        "body/over-the-limit-then-a-valid-request",
        vec![limited("POST").body(bytes(100)), limited("GET")],
    )
    .await;
    c.run(
        "body/service-limit-reached",
        vec![Req::new("POST", "/p/x").body(bytes(64 * 1024))],
    )
    .await;
    c.run(
        "body/service-limit-exceeded",
        vec![Req::new("POST", "/p/x").body(bytes(64 * 1024 + 1))],
    )
    .await;
    c.run(
        "body/bound-is-not-applied-to-fixed-routes",
        vec![Req::new("POST", "/fixed/ok").body(bytes(100 * 1024))],
    )
    .await;
}

async fn content_length(c: &mut Context) {
    let post = |value: &str| {
        Req::new("POST", "/p/echo")
            .header("content-length", value)
            .text("12345")
    };
    for (name, value) in [
        ("not-a-number", "abc"),
        ("negative", "-1"),
        ("plus-sign", "+5"),
        ("trailing-space", "5 "),
        ("empty", ""),
    ] {
        c.run(&format!("content-length/{name}"), vec![post(value)])
            .await;
    }
    c.run(
        "content-length/duplicated-equal",
        vec![post("5").header("content-length", "5")],
    )
    .await;
    c.run(
        "content-length/duplicated-different",
        vec![post("5").header("content-length", "6")],
    )
    .await;
    c.run("content-length/list", vec![post("5, 5")]).await;
    c.run("content-length/shorter-than-the-body", vec![post("3")])
        .await;
    c.run("content-length/longer-than-the-body", vec![post("9")])
        .await;
    c.run("content-length/leading-zeros", vec![post("005")])
        .await;
    c.run(
        "content-length/on-a-bodyless-get",
        vec![Req::get("/p/echo").header("content-length", "0")],
    )
    .await;
}

async fn invalid_paths(c: &mut Context) {
    let paths = [
        ("double-slash", "/p//double"),
        ("dot-segment", "/p/./dot"),
        ("dot-dot-segment", "/p/../dotdot"),
        ("encoded-dot-dot", "/p/%2e%2e/x"),
        ("encoded-slash", "/p/a%2fb"),
        ("invalid-percent-escape", "/p/%zz"),
        ("encoded-unreserved-letter", "/p/%41"),
        ("encoded-space", "/p/a%20b"),
        ("encoded-utf8", "/p/%E2%9C%93"),
    ];
    for (name, path) in paths {
        c.run(
            &format!("path/{name}"),
            vec![Req::get(path), Req::get("/fixed/ok")],
        )
        .await;
    }
    c.run(
        "path/static-dot-segments",
        vec![
            Req::get("/static/../static/ok.txt"),
            Req::get("/static/sub/../ok.txt"),
        ],
    )
    .await;
}

/// A request whose field section is exactly `size` bytes by RFC 9114 4.2.2.
fn sized(host: &str, size: usize) -> Vec<u8> {
    let fields = raw::with(raw::base(host, "/fixed/ok"), &[("x-pad", "")]);
    let padding = size - raw::section_size(&fields);
    raw::headers(&raw::with(
        raw::base(host, "/fixed/ok"),
        &[("x-pad", &"a".repeat(padding))],
    ))
}

async fn header_sections(c: &mut Context) {
    let too_large = || {
        Tolerance::new("the body and headers of a 431 answer are the server's own")
            .ignore("steps[0].outcome.body")
            .ignore("steps[0].outcome.headers")
    };
    let id = "header-section/h3-client-20000-byte-field";
    let long = Req::get("/fixed/ok").header("x-padding", &"x".repeat(20_000));
    let reason = "the client refuses locally once it has seen the advertised SETTINGS_MAX_FIELD_SECTION_SIZE; otherwise the server answers 431";
    let tolerance = Tolerance::new(reason)
        .outcome(0, json!({ "status": 431, "headers": [], "body": "" }))
        .ignore("steps[0].outcome.body")
        .ignore("steps[0].outcome.headers");
    c.run_tolerant(id, vec![long], tolerance).await;
    for size in [16_000, 17_000, 20_000, 70_000] {
        let id = format!("header-section/size-{size}");
        let tolerance = (size > SECTION_LIMIT).then(too_large);
        request_case(
            c,
            &id,
            &format!("field section of {size} bytes"),
            &sized(&host(&id), size),
            tolerance,
        )
        .await;
    }
    for size in [SECTION_LIMIT, SECTION_LIMIT + 1] {
        let id = format!("header-section/boundary-{size}");
        let reason = "a field section up to the advertised size is accepted and a larger one may be answered 431 (RFC 9114 4.2.2)";
        let alternative = json!({ "status": if size == SECTION_LIMIT { 431 } else { 200 }, "headers": [], "body": "" });
        let tolerance = Tolerance::new(reason)
            .outcome(0, alternative)
            .ignore("steps[0].outcome.body")
            .ignore("steps[0].outcome.headers");
        request_case(
            c,
            &id,
            &format!("field section of {size} bytes"),
            &sized(&host(&id), size),
            Some(tolerance),
        )
        .await;
    }
    let id = "header-section/many-small-fields";
    let extra = (0..500).map(|index| (format!("x-{index:03}"), "v".to_owned()));
    let fields = raw::base(&host(id), "/fixed/ok")
        .into_iter()
        .chain(extra)
        .collect();
    request_case(
        c,
        id,
        "500 small fields",
        &raw::headers(&fields),
        Some(too_large()),
    )
    .await;
}

fn malformed(reason: &str) -> Option<Tolerance> {
    let reset = json!({ "step": 0, "outcome": { "stream_reset": 0x10e }, "origin": null, "connection_after": "open" });
    Some(Tolerance::new(reason).accept(reset))
}

async fn malformed_requests(c: &mut Context) {
    let reason = "malformed request (RFC 9114 4.1.2): a stream reset with H3_MESSAGE_ERROR or a 4xx answer both conform";
    let strings = |fields: &[(&str, &str)]| {
        fields
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect::<raw::Fields>()
    };
    let cases: Vec<(&str, raw::Fields)> = vec![
        (
            "missing-authority",
            strings(&[
                (":method", "GET"),
                (":scheme", "https"),
                (":path", "/fixed/ok"),
            ]),
        ),
        (
            "missing-method",
            strings(&[
                (":scheme", "https"),
                (":authority", "m.test"),
                (":path", "/fixed/ok"),
            ]),
        ),
        (
            "missing-scheme",
            strings(&[
                (":method", "GET"),
                (":authority", "m.test"),
                (":path", "/fixed/ok"),
            ]),
        ),
        (
            "missing-path",
            strings(&[
                (":method", "GET"),
                (":scheme", "https"),
                (":authority", "m.test"),
            ]),
        ),
        (
            "empty-path",
            strings(&[
                (":method", "GET"),
                (":scheme", "https"),
                (":authority", "m.test"),
                (":path", ""),
            ]),
        ),
        (
            "path-without-a-slash",
            strings(&[
                (":method", "GET"),
                (":scheme", "https"),
                (":authority", "m.test"),
                (":path", "fixed/ok"),
            ]),
        ),
        (
            "duplicate-method",
            strings(&[
                (":method", "GET"),
                (":method", "GET"),
                (":scheme", "https"),
                (":authority", "m.test"),
                (":path", "/fixed/ok"),
            ]),
        ),
        (
            "pseudo-after-regular",
            strings(&[
                (":method", "GET"),
                (":scheme", "https"),
                ("x-a", "1"),
                (":authority", "m.test"),
                (":path", "/fixed/ok"),
            ]),
        ),
        (
            "unknown-pseudo-field",
            strings(&[
                (":method", "GET"),
                (":scheme", "https"),
                (":authority", "m.test"),
                (":path", "/fixed/ok"),
                (":foo", "x"),
            ]),
        ),
        (
            "response-pseudo-field",
            strings(&[
                (":method", "GET"),
                (":scheme", "https"),
                (":authority", "m.test"),
                (":path", "/fixed/ok"),
                (":status", "200"),
            ]),
        ),
        (
            "uppercase-field-name",
            raw::with(raw::base("m.test", "/fixed/ok"), &[("X-Upper", "1")]),
        ),
        (
            "connection-field",
            raw::with(raw::base("m.test", "/fixed/ok"), &[("connection", "close")]),
        ),
    ];
    for (name, fields) in cases {
        let id = format!("malformed/{name}");
        let fields = fields
            .into_iter()
            .map(|(n, v)| (n, if v == "m.test" { host(&id) } else { v }))
            .collect();
        request_case(c, &id, name, &raw::headers(&fields), malformed(reason)).await;
    }
    let id = "malformed/host-field-instead-of-authority";
    let fields = strings(&[
        (":method", "GET"),
        (":scheme", "https"),
        (":path", "/fixed/ok"),
        ("host", &host(id)),
    ]);
    request_case(
        c,
        id,
        "host field without :authority",
        &raw::headers(&fields),
        None,
    )
    .await;
    let id = "malformed/authority-differs-from-host";
    let fields = raw::with(
        raw::base(&host(id), "/fixed/ok"),
        &[("host", "elsewhere.test")],
    );
    let reason = "differing :authority and host fields are unspecified (RFC 9114 4.3.1): the request is served or reset";
    let tolerance = Tolerance::new(reason)
        .ignore("steps[0].outcome")
        .ignore("steps[0].origin");
    request_case(
        c,
        id,
        ":authority and host disagree",
        &raw::headers(&fields),
        Some(tolerance),
    )
    .await;
    let id = "malformed/options-asterisk";
    let fields = strings(&[
        (":method", "OPTIONS"),
        (":scheme", "https"),
        (":path", "*"),
        (":authority", &host(id)),
    ]);
    request_case(
        c,
        id,
        "OPTIONS *",
        &raw::headers(&fields),
        malformed(reason),
    )
    .await;
    let id = "malformed/connect-request";
    let fields = strings(&[(":method", "CONNECT"), (":authority", &host(id))]);
    request_case(
        c,
        id,
        "CONNECT without scheme and path",
        &raw::headers(&fields),
        malformed(reason),
    )
    .await;
}

fn post_headers(host: &str, index: usize) -> Vec<u8> {
    raw::headers(&raw::with(
        raw::request("POST", host, &format!("/p/pending/{index}")),
        &[("content-length", "1")],
    ))
}

async fn stream_limits(c: &mut Context) {
    let (endpoint, address) = (c.endpoint.clone(), c.addresses.listener);
    let id = "limits/bidirectional-stream-limit";
    let raw = Raw::open(&endpoint, address).await;
    let mut streams = Vec::new();
    for index in 0..128 {
        streams.push(raw.request(&post_headers(&host(id), index)).await);
    }
    let blocked = timeout(BLOCKED, raw.conn.open_bi()).await.is_err();
    let outcome = json!({ "streams_opened": streams.len(), "next_stream_blocked": blocked });
    let tolerance = Tolerance::new(
        "requests cancelled when the connection closes are logged or not depending on timing",
    );
    c.record_step(
        id,
        "128 pending requests, then one more stream",
        outcome,
        Some(tolerance),
    );
    raw.finish();
    drop(streams);
    let id = "limits/unidirectional-stream-limit";
    let raw = Raw::open(&endpoint, address).await;
    let mut opened = 1;
    let mut streams = Vec::new();
    while opened < 64 {
        match timeout(BLOCKED, raw.conn.open_uni()).await {
            Ok(Ok(stream)) => streams.push(stream),
            _ => break,
        }
        opened += 1;
    }
    let outcome = json!({ "streams_opened_with_the_control_stream": opened });
    conclude(
        c,
        id,
        "unidirectional streams until opening blocks",
        raw,
        outcome,
        None,
    )
    .await;
    drop(streams);
}

async fn connection_limit(c: &mut Context) {
    c.wait_idle().await;
    let (endpoint, address) = (c.endpoint.clone(), c.addresses.listener);
    let id = "limits/listener-connection-limit";
    let mut held = Vec::new();
    let mut statuses = Vec::new();
    for _ in 0..LISTENER_CONNECTIONS {
        let raw = Raw::open(&endpoint, address).await;
        statuses.push(
            raw.exchange(&crate::cases_wire::ok_request(&host(id)))
                .await["status"]
                .clone(),
        );
        held.push(raw);
    }
    let extra = crate::client::connect_quic(&endpoint, address).await;
    let closed = match timeout(Duration::from_secs(30), extra.closed()).await {
        Ok(error) => raw::close_json(&error),
        Err(_) => json!({ "connection_open": true }),
    };
    let rejected = stats::settled(c.addresses.management).await["rejected_connections"].clone();
    let step = |request: &str, outcome: Value| (request.to_owned(), outcome);
    let mut steps = vec![
        step(
            "the connections up to the limit are served",
            json!({ "statuses": statuses }),
        ),
        step("one more connection", closed),
        step(
            "rejected connections counted",
            json!({ "rejected_connections": rejected }),
        ),
    ];
    held.pop().expect("held connection").finish();
    c.wait_idle_below(LISTENER_CONNECTIONS).await;
    let fresh = Raw::open(&endpoint, address).await;
    let answer = fresh
        .exchange(&crate::cases_wire::ok_request(&host(id)))
        .await;
    steps.push(step("a connection after one was released", answer));
    fresh.finish();
    let mut recorded = Vec::new();
    for (request, outcome) in steps {
        recorded.push(c.observe(&request, outcome).await);
    }
    for raw in held {
        raw.finish();
    }
    let reason = "the reaction to a connection over the listener limit is a server choice (RFC 9114 8.1 lists H3_NO_ERROR and H3_EXCESSIVE_LOAD)";
    let tolerance = Tolerance::new(reason).outcome(1, json!({ "connection_closed": 0x107 }));
    c.record(id, vec![host(id)], recorded, Some(tolerance));
}

/// A response whose headers exceed the bound cannot be sent: the exchange ends without one.
async fn unsendable_response(c: &mut Context) {
    let id = "fixed/oversized-response-header";
    let tolerance = Tolerance::new(
        "a response that cannot be sent: which error code the stream is reset with is not mandated",
    )
    .outcome(0, json!({ "stream_reset": 0x102 }))
    .outcome(0, json!({ "stream_reset": 0x100 }));
    request_case(
        c,
        id,
        "GET /fixed/oversized-header",
        &raw::headers(&raw::base(&host(id), "/fixed/oversized-header")),
        Some(tolerance),
    )
    .await;
}
