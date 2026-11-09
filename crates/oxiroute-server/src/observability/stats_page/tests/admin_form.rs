use http::{
    HeaderMap, HeaderValue,
    header::{HOST, ORIGIN, REFERER},
};

use crate::observability::stats_page::admin_form::{
    authority_is_loopback, parse_stats_form, request_is_same_origin,
};

#[test]
fn stats_page_form_and_same_origin_checks_fail_closed() {
    let target = parse_stats_form(
        b"generation_revision=abc&pool=pool%20one&server=node%2B1&state=maintenance",
    )
    .expect("form");
    assert_eq!(target.pool, "pool one");
    assert_eq!(target.server, "node+1");
    assert!(parse_stats_form(b"pool=one&pool=two").is_err());
    assert!(
        parse_stats_form(b"generation_revision=abc&pool=one&server=two&state=ready&extra=no")
            .is_err()
    );

    let mut headers = HeaderMap::new();
    headers.insert(HOST, HeaderValue::from_static("localhost:8404"));
    headers.insert(ORIGIN, HeaderValue::from_static("http://localhost:8404"));
    assert!(request_is_same_origin(&headers));
    headers.remove(ORIGIN);
    headers.insert(
        REFERER,
        HeaderValue::from_static("http://localhost:8404/stats"),
    );
    assert!(request_is_same_origin(&headers));
    headers.insert(ORIGIN, HeaderValue::from_static("http://localhost:8404"));
    headers.insert("forwarded", HeaderValue::from_static("for=127.0.0.1"));
    assert!(!request_is_same_origin(&headers));
    headers.remove("forwarded");
    headers.insert(ORIGIN, HeaderValue::from_static("http://attacker.test"));
    assert!(!request_is_same_origin(&headers));
    headers.insert(HOST, HeaderValue::from_static("attacker.test:8404"));
    headers.insert(
        ORIGIN,
        HeaderValue::from_static("http://attacker.test:8404"),
    );
    assert!(!request_is_same_origin(&headers));
    headers.insert(HOST, HeaderValue::from_static("127.0.0.1:8404"));
    headers.insert(ORIGIN, HeaderValue::from_static("http://127.0.0.1:8404"));
    assert!(request_is_same_origin(&headers));
    headers.insert(HOST, HeaderValue::from_static("localhost:not-a-port"));
    headers.insert(
        ORIGIN,
        HeaderValue::from_static("http://localhost:not-a-port"),
    );
    assert!(!request_is_same_origin(&headers));
    assert!(authority_is_loopback(
        &"[::1]:8404".parse().expect("IPv6 loopback authority")
    ));
    assert!(!authority_is_loopback(
        &"attacker@localhost:8404"
            .parse()
            .expect("userinfo authority")
    ));
}
