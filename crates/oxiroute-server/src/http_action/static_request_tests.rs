use std::time::Duration;

use http::Request;

use super::*;

struct DecisionCase {
    name: &'static str,
    headers: HeaderMap,
    etag_enabled: bool,
    size: u64,
    expected: StaticRequestDecision,
}

fn headers(values: &[(&str, &str)]) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (name, value) in values {
        headers.append(
            HeaderName::from_bytes(name.as_bytes()).expect("header name"),
            HeaderValue::from_str(value).expect("header value"),
        );
    }
    headers
}

fn plan(root: &Path, etag: bool) -> StaticFilesPlan {
    StaticFilesPlan {
        root: Arc::new(open_pinned_directory(root).expect("test static root")),
        directory_policy: StaticDirectoryPolicy::disabled(),
        fallback: None,
        mapping: HttpStaticPathMapping::Root,
        mount_path: "/".into(),
        directory_redirects: true,
        try_files: Box::new([]),
        etag,
        mime: HashMap::new(),
        default_type: None,
        headers: Box::new([]),
        error_responses: HashMap::new(),
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one table documents the complete protocol-neutral decision contract"
)]
fn static_request_decision_matrix_is_protocol_neutral() {
    let directory = tempfile::tempdir().expect("static decision directory");
    let path = directory.path().join("file.txt");
    std::fs::write(&path, b"0123456789").expect("static decision file");
    let enabled = plan(directory.path(), true);
    let disabled = plan(directory.path(), false);
    let modified = UNIX_EPOCH + Duration::from_millis(100_900);
    let same_second = httpdate::fmt_http_date(UNIX_EPOCH + Duration::from_secs(100));
    let older = httpdate::fmt_http_date(UNIX_EPOCH + Duration::from_secs(99));
    let newer = httpdate::fmt_http_date(UNIX_EPOCH + Duration::from_secs(101));
    let serve = StaticRequestDecision::Serve { range: None };
    let partial = |start, end| StaticRequestDecision::Serve {
        range: Some((start, end)),
    };
    let mut duplicate_range = headers(&[("range", "bytes=0-1")]);
    duplicate_range.append(RANGE, HeaderValue::from_static("bytes=2-3"));
    let mut invalid_range = HeaderMap::new();
    invalid_range.insert(
        RANGE,
        HeaderValue::from_bytes(b"bytes=\xff").expect("opaque invalid range value"),
    );
    let cases = vec![
        DecisionCase {
            name: "no conditions",
            headers: HeaderMap::new(),
            etag_enabled: true,
            size: 10,
            expected: serve,
        },
        DecisionCase {
            name: "strong If-Match",
            headers: headers(&[("if-match", "\"etag\"")]),
            etag_enabled: true,
            size: 10,
            expected: serve,
        },
        DecisionCase {
            name: "strong If-Match list",
            headers: headers(&[("if-match", "\"other\", \"etag\"")]),
            etag_enabled: true,
            size: 10,
            expected: serve,
        },
        DecisionCase {
            name: "weak If-Match fails",
            headers: headers(&[("if-match", "W/\"etag\"")]),
            etag_enabled: true,
            size: 10,
            expected: StaticRequestDecision::PreconditionFailed,
        },
        DecisionCase {
            name: "stale If-Match fails before If-None-Match",
            headers: headers(&[("if-match", "\"stale\""), ("if-none-match", "\"etag\"")]),
            etag_enabled: true,
            size: 10,
            expected: StaticRequestDecision::PreconditionFailed,
        },
        DecisionCase {
            name: "If-Match suppresses If-Unmodified-Since",
            headers: headers(&[("if-match", "\"etag\""), ("if-unmodified-since", &older)]),
            etag_enabled: true,
            size: 10,
            expected: serve,
        },
        DecisionCase {
            name: "matching If-Match continues to If-None-Match",
            headers: headers(&[("if-match", "\"etag\""), ("if-none-match", "\"etag\"")]),
            etag_enabled: true,
            size: 10,
            expected: StaticRequestDecision::NotModified,
        },
        DecisionCase {
            name: "stale If-Unmodified-Since",
            headers: headers(&[("if-unmodified-since", &older)]),
            etag_enabled: true,
            size: 10,
            expected: StaticRequestDecision::PreconditionFailed,
        },
        DecisionCase {
            name: "same-second If-Unmodified-Since",
            headers: headers(&[("if-unmodified-since", &same_second)]),
            etag_enabled: true,
            size: 10,
            expected: serve,
        },
        DecisionCase {
            name: "invalid If-Unmodified-Since",
            headers: headers(&[("if-unmodified-since", "invalid")]),
            etag_enabled: true,
            size: 10,
            expected: serve,
        },
        DecisionCase {
            name: "strong If-None-Match",
            headers: headers(&[("if-none-match", "\"etag\"")]),
            etag_enabled: true,
            size: 10,
            expected: StaticRequestDecision::NotModified,
        },
        DecisionCase {
            name: "weak If-None-Match",
            headers: headers(&[("if-none-match", "W/\"etag\"")]),
            etag_enabled: true,
            size: 10,
            expected: StaticRequestDecision::NotModified,
        },
        DecisionCase {
            name: "If-None-Match list",
            headers: headers(&[("if-none-match", "\"other\", W/\"etag\"")]),
            etag_enabled: true,
            size: 10,
            expected: StaticRequestDecision::NotModified,
        },
        DecisionCase {
            name: "nonmatching If-None-Match suppresses If-Modified-Since",
            headers: headers(&[
                ("if-none-match", "\"other\""),
                ("if-modified-since", &same_second),
            ]),
            etag_enabled: true,
            size: 10,
            expected: serve,
        },
        DecisionCase {
            name: "same-second If-Modified-Since",
            headers: headers(&[("if-modified-since", &same_second)]),
            etag_enabled: true,
            size: 10,
            expected: StaticRequestDecision::NotModified,
        },
        DecisionCase {
            name: "older If-Modified-Since",
            headers: headers(&[("if-modified-since", &older)]),
            etag_enabled: true,
            size: 10,
            expected: serve,
        },
        DecisionCase {
            name: "newer If-Modified-Since",
            headers: headers(&[("if-modified-since", &newer)]),
            etag_enabled: true,
            size: 10,
            expected: StaticRequestDecision::NotModified,
        },
        DecisionCase {
            name: "invalid If-Modified-Since",
            headers: headers(&[("if-modified-since", "invalid")]),
            etag_enabled: true,
            size: 10,
            expected: serve,
        },
        DecisionCase {
            name: "exact range",
            headers: headers(&[("range", "bytes=2-5")]),
            etag_enabled: true,
            size: 10,
            expected: partial(2, 5),
        },
        DecisionCase {
            name: "open range",
            headers: headers(&[("range", "bytes=7-")]),
            etag_enabled: true,
            size: 10,
            expected: partial(7, 9),
        },
        DecisionCase {
            name: "suffix range",
            headers: headers(&[("range", "bytes=-3")]),
            etag_enabled: true,
            size: 10,
            expected: partial(7, 9),
        },
        DecisionCase {
            name: "oversized suffix range",
            headers: headers(&[("range", "bytes=-20")]),
            etag_enabled: true,
            size: 10,
            expected: partial(0, 9),
        },
        DecisionCase {
            name: "range end is clamped",
            headers: headers(&[("range", "bytes=8-20")]),
            etag_enabled: true,
            size: 10,
            expected: partial(8, 9),
        },
        DecisionCase {
            name: "unknown range unit is ignored",
            headers: headers(&[("range", "widgets=0-1")]),
            etag_enabled: true,
            size: 10,
            expected: serve,
        },
        DecisionCase {
            name: "duplicate ranges",
            headers: duplicate_range,
            etag_enabled: true,
            size: 10,
            expected: StaticRequestDecision::RangeNotSatisfiable,
        },
        DecisionCase {
            name: "multiple ranges",
            headers: headers(&[("range", "bytes=0-1,2-3")]),
            etag_enabled: true,
            size: 10,
            expected: StaticRequestDecision::RangeNotSatisfiable,
        },
        DecisionCase {
            name: "opaque invalid range",
            headers: invalid_range,
            etag_enabled: true,
            size: 10,
            expected: StaticRequestDecision::RangeNotSatisfiable,
        },
        DecisionCase {
            name: "empty range",
            headers: headers(&[("range", "bytes=")]),
            etag_enabled: true,
            size: 10,
            expected: StaticRequestDecision::RangeNotSatisfiable,
        },
        DecisionCase {
            name: "zero suffix range",
            headers: headers(&[("range", "bytes=-0")]),
            etag_enabled: true,
            size: 10,
            expected: StaticRequestDecision::RangeNotSatisfiable,
        },
        DecisionCase {
            name: "range starts past end",
            headers: headers(&[("range", "bytes=10-")]),
            etag_enabled: true,
            size: 10,
            expected: StaticRequestDecision::RangeNotSatisfiable,
        },
        DecisionCase {
            name: "reversed range",
            headers: headers(&[("range", "bytes=5-2")]),
            etag_enabled: true,
            size: 10,
            expected: StaticRequestDecision::RangeNotSatisfiable,
        },
        DecisionCase {
            name: "empty file without range",
            headers: HeaderMap::new(),
            etag_enabled: true,
            size: 0,
            expected: serve,
        },
        DecisionCase {
            name: "empty file with range",
            headers: headers(&[("range", "bytes=0-")]),
            etag_enabled: true,
            size: 0,
            expected: StaticRequestDecision::RangeNotSatisfiable,
        },
        DecisionCase {
            name: "strong If-Range",
            headers: headers(&[("range", "bytes=0-1"), ("if-range", "\"etag\"")]),
            etag_enabled: true,
            size: 10,
            expected: partial(0, 1),
        },
        DecisionCase {
            name: "stale strong If-Range",
            headers: headers(&[("range", "bytes=0-1"), ("if-range", "\"stale\"")]),
            etag_enabled: true,
            size: 10,
            expected: serve,
        },
        DecisionCase {
            name: "weak If-Range",
            headers: headers(&[("range", "bytes=0-1"), ("if-range", "W/\"etag\"")]),
            etag_enabled: true,
            size: 10,
            expected: serve,
        },
        DecisionCase {
            name: "same-second date If-Range",
            headers: headers(&[("range", "bytes=0-1"), ("if-range", &same_second)]),
            etag_enabled: true,
            size: 10,
            expected: partial(0, 1),
        },
        DecisionCase {
            name: "newer date If-Range",
            headers: headers(&[("range", "bytes=0-1"), ("if-range", &newer)]),
            etag_enabled: true,
            size: 10,
            expected: partial(0, 1),
        },
        DecisionCase {
            name: "older date If-Range",
            headers: headers(&[("range", "bytes=0-1"), ("if-range", &older)]),
            etag_enabled: true,
            size: 10,
            expected: serve,
        },
        DecisionCase {
            name: "invalid date If-Range",
            headers: headers(&[("range", "bytes=0-1"), ("if-range", "invalid")]),
            etag_enabled: true,
            size: 10,
            expected: serve,
        },
        DecisionCase {
            name: "ETag-disabled wildcard If-Match",
            headers: headers(&[("if-match", "*")]),
            etag_enabled: false,
            size: 10,
            expected: serve,
        },
        DecisionCase {
            name: "ETag-disabled specific If-Match",
            headers: headers(&[("if-match", "\"etag\"")]),
            etag_enabled: false,
            size: 10,
            expected: StaticRequestDecision::PreconditionFailed,
        },
        DecisionCase {
            name: "ETag-disabled wildcard If-None-Match",
            headers: headers(&[("if-none-match", "*")]),
            etag_enabled: false,
            size: 10,
            expected: StaticRequestDecision::NotModified,
        },
        DecisionCase {
            name: "ETag-disabled specific If-None-Match suppresses date",
            headers: headers(&[
                ("if-none-match", "\"etag\""),
                ("if-modified-since", &same_second),
            ]),
            etag_enabled: false,
            size: 10,
            expected: serve,
        },
        DecisionCase {
            name: "ETag-disabled If-Modified-Since",
            headers: headers(&[("if-modified-since", &same_second)]),
            etag_enabled: false,
            size: 10,
            expected: StaticRequestDecision::NotModified,
        },
        DecisionCase {
            name: "ETag-disabled If-Range",
            headers: headers(&[("range", "bytes=0-1"), ("if-range", "\"etag\"")]),
            etag_enabled: false,
            size: 10,
            expected: serve,
        },
        DecisionCase {
            name: "ETag-disabled date If-Range",
            headers: headers(&[("range", "bytes=0-1"), ("if-range", &same_second)]),
            etag_enabled: false,
            size: 10,
            expected: partial(0, 1),
        },
    ];

    let mut file = StaticFile {
        etag: HeaderValue::from_static("\"etag\""),
        file: File::open(path).expect("open static decision file"),
        modified,
        name: OsString::from("file.txt"),
        size: 10,
    };
    for case in cases {
        file.size = case.size;
        let plan = if case.etag_enabled {
            &enabled
        } else {
            &disabled
        };
        for method in [Method::GET, Method::HEAD] {
            let mut request = Request::builder()
                .method(method.clone())
                .body(())
                .expect("static request");
            *request.headers_mut() = case.headers.clone();
            assert_eq!(
                plan.request_decision(request.headers(), &file),
                case.expected,
                "{} for {method}",
                case.name
            );
        }
    }
}
