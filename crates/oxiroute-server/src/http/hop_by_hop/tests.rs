use super::*;

fn parsed(version: Version, fields: &[(&str, &str)]) -> (RequestHeader, RequestFacts) {
    let mut request = RequestHeader::build_no_case("GET", b"/", None).expect("request");
    request.set_version(version);
    for (name, value) in fields {
        request
            .append_header(name.to_string(), *value)
            .expect("field");
    }
    let facts = RequestFacts::of(&request.headers);
    (request, facts)
}

fn names(request: &RequestHeader) -> Vec<String> {
    request
        .headers
        .keys()
        .map(|name| name.as_str().to_owned())
        .collect()
}

fn headers(fields: &[(&str, &str)]) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (name, value) in fields {
        headers.append(
            HeaderName::from_bytes(name.as_bytes()).expect("name"),
            value.parse().expect("value"),
        );
    }
    headers
}

#[test]
fn options_are_collected_across_fields_ignoring_case_whitespace_and_empty_elements() {
    let parsed = ConnectionOptions::parse(&headers(&[
        ("connection", " , X-One ,,\tx-two,"),
        ("connection", "CLOSE"),
        ("connection", "Keep-Alive, upgrade"),
    ]))
    .expect("options");
    assert!(parsed.close && parsed.upgrade);
    let expected = [
        HeaderName::from_static("x-one"),
        HeaderName::from_static("x-two"),
    ];
    assert_eq!(parsed.nominated, expected);
}

#[test]
fn options_that_name_framing_or_are_not_tokens_are_refused() {
    for value in [
        "content-length",
        "Transfer-Encoding",
        "x, HOST",
        "two words",
        "a;b",
    ] {
        assert_eq!(
            ConnectionOptions::parse(&headers(&[("connection", value)])),
            Err(InvalidNomination),
            "{value}"
        );
    }
    let mut invalid = HeaderMap::new();
    invalid.append(
        CONNECTION,
        HeaderValue::from_bytes(b"x\xff").expect("obs-text"),
    );
    assert_eq!(ConnectionOptions::parse(&invalid), Err(InvalidNomination));
}

#[test]
fn a_request_without_connection_or_hop_by_hop_fields_is_not_touched() {
    let (mut request, facts) = parsed(Version::HTTP_11, &[("host", "a"), ("x-keep", "1")]);
    assert_eq!(strip_request(&mut request, facts, false), Ok(facts));
    assert_eq!(names(&request), ["host", "x-keep"]);
}

#[test]
fn nominated_and_fixed_fields_are_removed_and_the_rest_stays() {
    let (mut request, facts) = parsed(
        Version::HTTP_11,
        &[
            ("host", "a"),
            ("connection", "keep-alive, X-Nominated"),
            ("x-nominated", "1"),
            ("te", "trailers"),
            ("proxy-authorization", "x"),
            ("keep-alive", "5"),
            ("trailer", "x"),
            ("transfer-encoding", "chunked"),
            ("authorization", "kept"),
        ],
    );
    let facts = strip_request(&mut request, facts, false).expect("stripped");
    assert_eq!(
        names(&request),
        ["host", "transfer-encoding", "authorization"]
    );
    assert!(!facts.connection && !facts.hop_by_hop && !facts.upgrade);
    assert!(facts.transfer_encoding);
    assert_eq!(RequestFacts::of(&request.headers), facts);
}

#[test]
fn the_usual_connection_of_a_client_is_handled_without_a_scan() {
    let (mut request, facts) = parsed(
        Version::HTTP_11,
        &[("host", "a"), ("connection", "Keep-Alive")],
    );
    let facts = strip_request(&mut request, facts, false).expect("stripped");
    assert_eq!(names(&request), ["host"]);
    assert_eq!(RequestFacts::of(&request.headers), facts);

    let (mut request, facts) = parsed(Version::HTTP_11, &[("host", "a"), ("connection", "Close")]);
    assert_eq!(strip_request(&mut request, facts, false), Ok(facts));
    assert_eq!(request.headers[CONNECTION], "Close");
}

#[test]
fn close_stays_as_the_only_connection_option() {
    let (mut request, facts) = parsed(
        Version::HTTP_11,
        &[("connection", "x-gone, close"), ("x-gone", "1")],
    );
    let facts = strip_request(&mut request, facts, false).expect("stripped");
    assert_eq!(request.headers[CONNECTION], "close");
    assert!(facts.connection);
    assert_eq!(RequestFacts::of(&request.headers), facts);
}

#[test]
fn a_valid_handshake_keeps_upgrade_and_a_canonical_connection() {
    let fields = [
        ("connection", "keep-alive, upgrade, x-gone"),
        ("upgrade", "websocket"),
        ("x-gone", "1"),
    ];
    let (mut request, facts) = parsed(Version::HTTP_11, &fields);
    let facts = strip_request(&mut request, facts, false).expect("stripped");
    assert_eq!(request.headers[CONNECTION], "Upgrade");
    assert_eq!(request.headers[UPGRADE], "websocket");
    assert!(facts.upgrade && facts.connection);
    let mut kept = names(&request);
    kept.sort();
    assert_eq!(kept, ["connection", "upgrade"]);
}

#[test]
fn upgrade_without_the_connection_option_or_over_http_1_0_is_removed() {
    for (version, connection) in [
        (Version::HTTP_11, "keep-alive"),
        (Version::HTTP_10, "upgrade"),
    ] {
        let (mut request, facts) = parsed(
            version,
            &[("connection", connection), ("upgrade", "websocket")],
        );
        let facts = strip_request(&mut request, facts, false).expect("stripped");
        assert!(!request.headers.contains_key(UPGRADE));
        assert!(!facts.upgrade);
    }
    let (mut request, facts) = parsed(Version::HTTP_11, &[("upgrade", "websocket")]);
    strip_request(&mut request, facts, false).expect("stripped");
    assert!(request.headers.is_empty());
}

#[test]
fn te_trailers_of_an_http2_request_stays() {
    let fields = [
        ("te", "trailers"),
        ("proxy-authorization", "x"),
        ("trailer", "y"),
    ];
    let (mut request, _) = parsed(Version::HTTP_2, &fields);
    strip_request(&mut request, RequestFacts::UNKNOWN, true).expect("stripped");
    assert_eq!(names(&request), ["te"]);
}

#[test]
fn a_refused_request_is_left_unchanged() {
    let (mut request, facts) = parsed(
        Version::HTTP_11,
        &[("connection", "x, content-length"), ("x", "1"), ("te", "a")],
    );
    assert_eq!(
        strip_request(&mut request, facts, false),
        Err(InvalidNomination)
    );
    assert_eq!(names(&request), ["connection", "x", "te"]);
}

#[test]
fn hop_by_hop_removals_are_the_present_nominated_and_fixed_fields() {
    let response = headers(&[
        ("connection", "keep-alive, x-private, x-absent"),
        ("x-private", "1"),
        ("keep-alive", "timeout=5"),
        ("upgrade", "h2c"),
        ("content-length", "2"),
    ]);
    assert!(has_hop_by_hop(&response));
    let removals = hop_by_hop_removals(&response).expect("removals");
    let expected =
        ["x-private", "connection", "keep-alive", "upgrade"].map(HeaderName::from_static);
    assert_eq!(removals, expected);
    assert!(!has_hop_by_hop(&headers(&[("content-length", "2")])));
    assert_eq!(
        hop_by_hop_removals(&headers(&[("connection", "content-length")])),
        Err(InvalidNomination)
    );
}
