//! The request the lane writes to the origin, against the classic sequence on the same request: stamp the id, append
//! the `Host` of the authority, replace it with the selected server's, `Connection: close` for a pool that never
//! reuses connections, then the wire writer of the vendored HTTP/1 client.

use http::{
    HeaderValue, Method, Request, Version,
    header::{CONNECTION, HOST},
};
use pingora::{http::RequestHeader, protocols::http::v1::client::http_req_header_to_wire};

use super::{head, request};
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
    let mut upstream = request.clone();
    stamp_request(&mut upstream).unwrap();
    upstream.set_version(Version::HTTP_11);
    let authority = upstream.uri.authority().unwrap().as_str().to_owned();
    upstream.insert_header(HOST, authority).unwrap();
    upstream.insert_header(HOST, host.to_owned()).unwrap();
    if close {
        upstream.insert_header(CONNECTION, "close").unwrap();
    }
    http_req_header_to_wire(&upstream).unwrap().to_vec()
}

/// What the lane writes, or `None` when it does not serve the request.
fn lane(request: &RequestHeader, host: &str, close: bool) -> Option<Vec<u8>> {
    let authority = request.uri.authority().unwrap();
    let (id, wire) = request::read(request, authority)?;
    let mut out = Vec::new();
    let host = HeaderValue::from_str(host).unwrap();
    head::render(
        &mut out,
        &wire,
        &head::edits(&wire, &id, Some(&host), close),
    );
    Some(out)
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
    let request = request_of(
        Method::GET,
        "/",
        &[
            ("cookie", "a=1"),
            ("accept", "x"),
            ("cookie", "b=2"),
            ("x-custom", "1"),
            ("cookie", "c=3"),
            ("x-custom", "2"),
        ],
    );
    same_head(&request, "example.com", false);
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
    let (id, _) = request::read(&kept, kept.uri.authority().unwrap()).unwrap();
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
    let (id, _) = request::read(&twice, twice.uri.authority().unwrap()).unwrap();
    assert_eq!(id, "one");
}

#[test]
fn a_request_with_a_field_the_lane_does_not_write_is_not_the_lanes() {
    for field in [
        ("host", "example.com"),
        ("te", "trailers"),
        ("trailer", "x"),
        ("expect", "100-continue"),
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
fn a_content_length_of_zero_goes_on_and_a_larger_one_is_not_the_lanes() {
    let empty = request_of(Method::POST, "/", &[("content-length", "0")]);
    same_head(&empty, "example.com", false);
    let body = request_of(Method::POST, "/", &[("content-length", "5")]);
    assert!(lane(&body, "example.com", false).is_none());
}
