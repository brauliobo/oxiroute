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
        cache: StaticCache::default(),
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
        content: StaticContent::Open(File::open(path).expect("open static decision file")),
        content_type: HeaderValue::from_static("text/plain; charset=utf-8"),
        etag: HeaderValue::from_static("\"etag\""),
        last_modified: HeaderValue::from_static("Thu, 01 Jan 1970 00:01:40 GMT"),
        modified,
        name: OsStr::new("file.txt").into(),
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

fn memory_file(body: &'static [u8]) -> StaticFile {
    StaticFile {
        content: StaticContent::Memory(Bytes::from_static(body)),
        content_type: HeaderValue::from_static("text/plain; charset=utf-8"),
        etag: HeaderValue::from_static("\"etag\""),
        last_modified: HeaderValue::from_static("Thu, 01 Jan 1970 00:01:40 GMT"),
        modified: UNIX_EPOCH,
        name: OsStr::new("file.txt").into(),
        size: body.len() as u64,
    }
}

#[tokio::test]
async fn small_files_are_served_from_memory_for_one_validity_window() {
    let directory = tempfile::tempdir().expect("static cache directory");
    let path = directory.path().join("hot.txt");
    std::fs::write(&path, b"first").expect("write file");
    let plan = plan(directory.path(), true);

    let StaticTarget::File(file) = plan.serve("/hot.txt").await.expect("first read") else {
        panic!("a file");
    };
    assert!(matches!(&file.content, StaticContent::Memory(body) if body.as_ref() == b"first"));

    std::fs::write(&path, b"second!").expect("rewrite file");
    let StaticTarget::File(file) = plan.serve("/hot.txt").await.expect("cached read") else {
        panic!("a file");
    };
    assert!(matches!(&file.content, StaticContent::Memory(body) if body.as_ref() == b"first"));

    std::fs::remove_file(&path).expect("remove file");
    assert!(
        plan.serve("/hot.txt").await.is_ok(),
        "removal is hidden inside the window"
    );
}

#[test]
fn cache_entries_expire_after_the_validity_window() {
    let cache = StaticCache::default();
    let start = Instant::now();
    cache.insert("/a", &memory_file(b"a"), start);
    assert!(
        cache
            .get("/a", start + Duration::from_millis(999))
            .is_some()
    );
    assert!(cache.get("/a", start + VALIDITY).is_none());
    assert!(cache.get("/b", start).is_none());
}

#[test]
fn a_full_cache_drops_expired_entries_and_otherwise_skips_new_ones() {
    let cache = StaticCache::default();
    let start = Instant::now();
    for index in 0..512 {
        cache.insert(&format!("/{index}"), &memory_file(b"x"), start);
    }
    cache.insert("/late", &memory_file(b"x"), start + VALIDITY / 2);
    assert!(
        cache.get("/late", start + VALIDITY / 2).is_none(),
        "full of fresh entries"
    );
    cache.insert("/late", &memory_file(b"x"), start + VALIDITY);
    assert!(
        cache.get("/late", start + VALIDITY).is_some(),
        "expired entries made room"
    );
    assert!(cache.get("/0", start + VALIDITY).is_none());
}

#[test]
fn files_over_the_memory_limit_are_not_cached() {
    let directory = tempfile::tempdir().expect("static cache directory");
    let path = directory.path().join("big.bin");
    std::fs::write(
        &path,
        vec![7; usize::try_from(MEMORY_FILE_BYTES).unwrap() + 1],
    )
    .expect("write");
    let content = StaticContent::Open(File::open(path).expect("open"));
    let loaded = StaticFile {
        content: content.load(MEMORY_FILE_BYTES + 1).expect("load"),
        ..memory_file(b"")
    };
    assert!(matches!(loaded.content, StaticContent::Open(_)));
    let cache = StaticCache::default();
    cache.insert("/big", &loaded, Instant::now());
    assert!(cache.get("/big", Instant::now()).is_none());
}

async fn collect(content: StaticContent, start: u64, length: u64) -> Vec<u8> {
    let mut reader = content.into_reader(start, length);
    let mut bytes = Vec::new();
    while let Some(chunk) = reader.next_chunk().await.expect("chunk") {
        bytes.extend_from_slice(&chunk);
    }
    bytes
}

#[tokio::test]
async fn memory_and_open_content_yield_the_same_ranges() {
    let directory = tempfile::tempdir().expect("static content directory");
    let path = directory.path().join("data.bin");
    let data: Vec<u8> = (0..200_000_u32).map(|value| (value % 251) as u8).collect();
    std::fs::write(&path, &data).expect("write data");
    for (start, length) in [
        (0, 200_000),
        (0, 1),
        (65_535, 65_538),
        (199_999, 1),
        (10, 0),
    ] {
        let (from, to) = (
            usize::try_from(start).unwrap(),
            usize::try_from(start + length).unwrap(),
        );
        let expected = &data[from..to];
        let open = StaticContent::Open(File::open(&path).expect("open"));
        assert_eq!(
            collect(open, start, length).await,
            expected,
            "open {start}+{length}"
        );
        let memory = StaticContent::Memory(Bytes::from(data.clone()));
        assert_eq!(
            collect(memory, start, length).await,
            expected,
            "memory {start}+{length}"
        );
    }
}

#[tokio::test]
async fn a_truncated_file_ends_the_stream_with_an_error() {
    let directory = tempfile::tempdir().expect("static content directory");
    let path = directory.path().join("short.bin");
    std::fs::write(&path, b"abc").expect("write data");
    let mut reader = StaticContent::Open(File::open(&path).expect("open")).into_reader(0, 10);
    assert!(reader.next_chunk().await.is_err());
}
