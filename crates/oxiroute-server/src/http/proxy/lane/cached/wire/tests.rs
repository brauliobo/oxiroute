use std::time::SystemTime;

use bytes::{Bytes, BytesMut};
use http::{HeaderMap, HeaderValue, Method, StatusCode};
use oxiroute_cache::{Cache, CacheConfig, FillJoin, FreshHit, RequestKeyInput, ResponseTiming};

use super::{Representation, body_sent, put};

fn hit(status: StatusCode, response: &[(&str, &str)], body: &'static [u8]) -> FreshHit {
    let cache = Cache::new(CacheConfig::default()).expect("cache");
    let mut headers = HeaderMap::new();
    headers.insert(
        http::header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=60"),
    );
    for (name, value) in response {
        headers.append(
            http::HeaderName::from_bytes(name.as_bytes()).expect("name"),
            HeaderValue::from_str(value).expect("value"),
        );
    }
    let request = RequestKeyInput {
        method: &Method::GET,
        scheme: "http",
        authority: "example.com",
        path: "/item",
        query: None,
        headers: &HeaderMap::new(),
    };
    let now = cache.now();
    let timing = ResponseTiming {
        request_started: now,
        response_received: now,
        response_received_wall: SystemTime::now(),
    };
    let entry = cache
        .prepare(
            request,
            status,
            &headers,
            Bytes::from_static(body),
            timing,
            &[],
        )
        .expect("prepared");
    let FillJoin::Leader(fill) = cache.begin_fill(entry.key().base().clone()).expect("fill") else {
        panic!("a leader");
    };
    fill.store(entry).expect("stored");
    cache.fresh_hit(request).expect("a fresh hit")
}

fn written(hit: &FreshHit) -> String {
    let mut out = BytesMut::new();
    put(&mut out, Representation::from(hit), (b"rq-1", "keep-alive"));
    let text = String::from_utf8(out.to_vec()).expect("text");
    text.split_inclusive("\r\n")
        .map(|line| {
            if line.starts_with("Date: ") {
                "Date: <date>\r\n"
            } else {
                line
            }
        })
        .collect()
}

#[test]
fn the_fields_the_writer_owns_are_added_in_the_order_the_classic_path_adds_them() {
    let hit = hit(StatusCode::OK, &[("etag", "\"a\""), ("x-a", "1")], b"body");
    assert_eq!(
        written(&hit),
        "HTTP/1.1 200 OK\r\nCache-Control: public, max-age=60\r\netag: \"a\"\r\nx-a: 1\r\nAge: 0\r\n\
         Content-Length: 4\r\nx-request-id: rq-1\r\nDate: <date>\r\nConnection: keep-alive\r\n\r\n"
    );
}

#[test]
fn the_stored_length_date_and_names_keep_their_place_and_their_case() {
    let date = httpdate::fmt_http_date(SystemTime::now());
    let hit = hit(
        StatusCode::OK,
        &[
            ("content-type", "text/plain"),
            ("content-length", "999"),
            ("date", &date),
            ("x-multi", "a"),
            ("x-multi", "b"),
        ],
        b"body",
    );
    assert_eq!(
        written(&hit),
        "HTTP/1.1 200 OK\r\nCache-Control: public, max-age=60\r\nContent-Type: text/plain\r\n\
         Content-Length: 4\r\nDate: <date>\r\nx-multi: a\r\nx-multi: b\r\nAge: 0\r\n\
         x-request-id: rq-1\r\nConnection: keep-alive\r\n\r\n"
    );
}

#[test]
fn a_status_without_a_body_has_no_length_and_sends_no_body() {
    let hit = hit(StatusCode::NO_CONTENT, &[("content-length", "0")], b"");
    assert_eq!(
        written(&hit),
        "HTTP/1.1 204 No Content\r\nCache-Control: public, max-age=60\r\nAge: 0\r\n\
         x-request-id: rq-1\r\nDate: <date>\r\nConnection: keep-alive\r\n\r\n"
    );
    assert_eq!(body_sent(Representation::from(&hit), false), b"");
}

#[test]
fn a_head_request_keeps_the_length_of_the_body_it_does_not_send() {
    let hit = hit(StatusCode::OK, &[], b"body");
    assert!(written(&hit).contains("Content-Length: 4\r\n"));
    assert_eq!(body_sent(Representation::from(&hit), true), b"");
    assert_eq!(body_sent(Representation::from(&hit), false), b"body");
}
