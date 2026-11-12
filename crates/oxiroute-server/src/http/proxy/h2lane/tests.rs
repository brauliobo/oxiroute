//! The request the lane writes to the origin, against the classic sequence on the same request: stamp the id, append
//! the `Host` of the authority, replace it with the selected server's, `Connection: close` for a pool that never
//! reuses connections, then the wire writer of the vendored HTTP/1 client.

use http::{
    HeaderValue, Method, Request, Version,
    header::{CONNECTION, HOST, TRANSFER_ENCODING},
};
use pingora::{http::RequestHeader, protocols::http::v1::client::http_req_header_to_wire};

use super::{claim::Body, head, request};
use crate::http::request_id::stamp_request;

fn request_of(method: Method, target: &str, fields: &[(&str, &str)]) -> RequestHeader {
    let mut builder = Request::builder()
        .method(method)
        .uri(format!("https://example.com{target}"))
        .version(Version::HTTP_2);
    for (name, value) in fields {
        builder = builder.header(*name, *value);
    }
    RequestHeader::from(builder.body(()).unwrap().into_parts().0)
}

/// What the classic path writes for the request.
fn classic(request: &RequestHeader, host: &str, close: bool) -> Vec<u8> {
    classic_body(request, host, close, false)
}

/// What the classic path writes for a request whose body has no announced length: chunked, after every filter.
fn classic_body(request: &RequestHeader, host: &str, close: bool, chunked: bool) -> Vec<u8> {
    let mut upstream = request.clone();
    stamp_request(&mut upstream).unwrap();
    upstream.set_version(Version::HTTP_11);
    let authority = upstream.uri.authority().unwrap().as_str().to_owned();
    upstream.insert_header(HOST, authority).unwrap();
    upstream.insert_header(HOST, host.to_owned()).unwrap();
    if close {
        upstream.insert_header(CONNECTION, "close").unwrap();
    }
    if chunked {
        upstream
            .insert_header(TRANSFER_ENCODING, "chunked")
            .unwrap();
    }
    http_req_header_to_wire(&upstream).unwrap().to_vec()
}

type WireFields<'a> = Vec<(&'a [u8], &'a [u8])>;

/// The fields as the classic header map holds them.
fn map_fields(request: &RequestHeader) -> WireFields<'_> {
    request
        .headers
        .iter()
        .map(|(name, value)| (name.as_str().as_bytes(), value.as_bytes()))
        .collect()
}

/// What the lane writes for a request whose fields arrived in the order `fields`, or `None` when it does not serve
/// it.
fn lane_fields(
    request: &RequestHeader,
    fields: &WireFields<'_>,
    host: &str,
    close: bool,
) -> Option<Vec<u8>> {
    lane_body(request, fields, host, close, Body::None)
}

/// Like `lane_fields` for a request whose body of `body` bytes follows the head.
fn lane_body(
    request: &RequestHeader,
    fields: &WireFields<'_>,
    host: &str,
    close: bool,
    body: Body,
) -> Option<Vec<u8>> {
    let authority = request.uri.authority().unwrap();
    let target = request.uri.path_and_query().unwrap().as_str().as_bytes();
    let method = request.method.as_str().as_bytes();
    let (id, wire) = request::read((method, target), &fields.iter().copied(), authority, body)?;
    let mut out = Vec::new();
    let host = HeaderValue::from_str(host).unwrap();
    let mut edits = head::edits(&wire, &id, Some(&host), close);
    if body == Body::Unknown {
        head::chunked(&mut edits);
    }
    head::render(&mut out, &wire, &edits);
    Some(out)
}

fn lane(request: &RequestHeader, host: &str, close: bool) -> Option<Vec<u8>> {
    lane_fields(request, &map_fields(request), host, close)
}

/// The lane's bytes equal the classic ones, but for the id: both stamp an id when the request has no single valid
/// one, and a generated id differs, so the stamped line is compared by its name.
fn same_head(request: &RequestHeader, host: &str, close: bool) {
    let expected = classic(request, host, close);
    let actual = lane(request, host, close).expect("the lane serves this request");
    assert_eq!(mask_generated(&actual), mask_generated(&expected));
}

fn mask_generated(head: &[u8]) -> String {
    String::from_utf8_lossy(head)
        .lines()
        .map(|line| {
            if line.starts_with("x-request-id: rq-") {
                "x-request-id: rq-<generated>"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn id_of(request: &RequestHeader) -> HeaderValue {
    let target = request.uri.path_and_query().unwrap().as_str().as_bytes();
    let fields = map_fields(request);
    let method = request.method.as_str().as_bytes();
    let (id, _) = request::read(
        (method, target),
        &fields.iter().copied(),
        request.uri.authority().unwrap(),
        Body::None,
    )
    .unwrap();
    id
}

#[test]
fn a_plain_request_is_written_as_the_classic_path_writes_it() {
    let request = request_of(
        Method::GET,
        "/a/b?c=d",
        &[
            ("accept", "*/*"),
            ("user-agent", "probe"),
            ("accept-encoding", "gzip"),
        ],
    );
    same_head(&request, "example.com", false);
    same_head(&request, "127.0.0.1:8080", false);
    same_head(&request, "127.0.0.1:8080", true);
}

#[test]
fn fields_of_one_name_are_grouped_where_the_header_map_puts_them() {
    let wire = [
        ("cookie", "a=1"),
        ("accept", "x"),
        ("cookie", "b=2"),
        ("x-custom", "1"),
        ("cookie", "c=3"),
        ("x-custom", "2"),
    ];
    let request = request_of(Method::GET, "/", &wire);
    let fields = wire
        .iter()
        .map(|(name, value)| (name.as_bytes(), value.as_bytes()))
        .collect();
    let actual = lane_fields(&request, &fields, "example.com", false).unwrap();
    let expected = classic(&request, "example.com", false);
    assert_eq!(mask_generated(&actual), mask_generated(&expected));
}

#[test]
fn names_the_classic_writer_capitalises_are_capitalised() {
    let request = request_of(
        Method::HEAD,
        "/",
        &[
            ("content-type", "text/plain"),
            ("cache-control", "no-cache"),
            ("content-length", "0"),
            ("accept-ranges", "none"),
        ],
    );
    same_head(&request, "example.com", false);
}

#[test]
fn a_valid_request_id_is_kept_and_any_other_is_replaced() {
    let kept = request_of(
        Method::GET,
        "/",
        &[("x-request-id", "abc-123"), ("accept", "*/*")],
    );
    same_head(&kept, "example.com", false);
    let id = id_of(&kept);
    assert_eq!(id, "abc-123");
    let invalid = request_of(
        Method::GET,
        "/",
        &[("accept", "*/*"), ("x-request-id", "no spaces allowed")],
    );
    same_head(&invalid, "example.com", false);
    let twice = request_of(
        Method::GET,
        "/",
        &[
            ("x-request-id", "one"),
            ("accept", "*/*"),
            ("x-request-id", "two"),
        ],
    );
    same_head(&twice, "example.com", false);
    let id = id_of(&twice);
    assert_eq!(id, "one");
}

#[test]
fn a_request_with_a_field_the_lane_does_not_write_is_not_the_lanes() {
    for field in [
        ("host", "example.com"),
        ("te", "trailers"),
        ("trailer", "x"),
        ("proxy-authorization", "Basic x"),
        ("proxy-authenticate", "Basic"),
        ("x-padded", " value"),
        ("x-padded", "value "),
    ] {
        let request = request_of(Method::GET, "/", &[field]);
        assert!(
            lane(&request, "example.com", false).is_none(),
            "{field:?} is for the classic path"
        );
    }
}

#[test]
fn a_content_length_of_zero_goes_on_and_a_larger_one_is_not_the_lanes_without_its_body() {
    let empty = request_of(Method::POST, "/", &[("content-length", "0")]);
    same_head(&empty, "example.com", false);
    let body = request_of(Method::POST, "/", &[("content-length", "5")]);
    assert!(lane(&body, "example.com", false).is_none());
}

#[test]
fn a_request_with_a_body_is_written_as_the_classic_path_writes_it() {
    let request = request_of(
        Method::POST,
        "/up?x=1",
        &[("content-type", "text/plain"), ("content-length", "5")],
    );
    let actual = lane_body(
        &request,
        &map_fields(&request),
        "example.com",
        false,
        Body::Length(5),
    )
    .expect("the lane serves a body the fields announce");
    assert_eq!(
        mask_generated(&actual),
        mask_generated(&classic(&request, "example.com", false))
    );
}

#[test]
fn a_body_that_the_fields_do_not_announce_is_not_the_lanes() {
    let request = request_of(Method::POST, "/", &[("content-length", "5")]);
    for body in [Body::None, Body::Length(4), Body::Length(6), Body::Unknown] {
        let lane = lane_body(&request, &map_fields(&request), "example.com", false, body);
        assert!(lane.is_none(), "a body of {body:?} against a length of 5");
    }
    let unannounced = request_of(Method::POST, "/", &[("accept", "*/*")]);
    assert!(
        lane_body(
            &unannounced,
            &map_fields(&unannounced),
            "example.com",
            false,
            Body::Length(5)
        )
        .is_none()
    );
}

#[test]
fn a_body_without_a_length_is_chunked_after_every_other_edit() {
    let request = request_of(
        Method::POST,
        "/up",
        &[("content-type", "text/plain"), ("x-request-id", "abc")],
    );
    for (host, close) in [("example.com", false), ("127.0.0.1:8080", true)] {
        let actual = lane_body(&request, &map_fields(&request), host, close, Body::Unknown)
            .expect("the lane serves a body of unknown length");
        assert_eq!(
            mask_generated(&actual),
            mask_generated(&classic_body(&request, host, close, true))
        );
    }
}

#[test]
fn an_expect_field_goes_on_to_the_origin_like_any_other() {
    let request = request_of(
        Method::POST,
        "/up",
        &[("expect", "100-continue"), ("content-length", "5")],
    );
    let actual = lane_body(
        &request,
        &map_fields(&request),
        "example.com",
        false,
        Body::Length(5),
    )
    .expect("the lane serves an expecting request");
    assert_eq!(
        mask_generated(&actual),
        mask_generated(&classic(&request, "example.com", false))
    );
}
