//! Requests that decide what a cache does with a stored response: conditions, ranges, the request's own cache
//! directives, credentials, and the heads a hit is written for (`HEAD`, `Connection: close`, two on one connection).

use super::{Case, HOST, add, get};

/// Request fields a cache reads, each on its own request.
const FIELDS: &[(&str, &str)] = &[
    ("if-none-match equal", "If-None-Match: \"v1\"\r\n"),
    ("if-none-match weak", "If-None-Match: W/\"v1\"\r\n"),
    ("if-none-match list", "If-None-Match: \"x\", \"v1\"\r\n"),
    ("if-none-match star", "If-None-Match: *\r\n"),
    ("if-none-match other", "If-None-Match: \"v2\"\r\n"),
    (
        "if-none-match twice",
        "If-None-Match: \"v2\"\r\nIf-None-Match: \"v1\"\r\n",
    ),
    (
        "if-modified-since after",
        "If-Modified-Since: Tue, 01 Jan 2030 00:00:00 GMT\r\n",
    ),
    (
        "if-modified-since before",
        "If-Modified-Since: Mon, 01 Jan 2001 00:00:00 GMT\r\n",
    ),
    (
        "if-modified-since invalid",
        "If-Modified-Since: yesterday\r\n",
    ),
    (
        "if-none-match wins over if-modified-since",
        "If-None-Match: \"v2\"\r\nIf-Modified-Since: Tue, 01 Jan 2030 00:00:00 GMT\r\n",
    ),
    ("if-match", "If-Match: \"v1\"\r\n"),
    (
        "if-unmodified-since",
        "If-Unmodified-Since: Tue, 01 Jan 2030 00:00:00 GMT\r\n",
    ),
    ("range", "Range: bytes=0-0\r\n"),
    (
        "range and if-range",
        "Range: bytes=0-0\r\nIf-Range: \"v1\"\r\n",
    ),
    ("if-range alone", "If-Range: \"v1\"\r\n"),
    ("no-cache", "Cache-Control: no-cache\r\n"),
    ("max-age zero", "Cache-Control: max-age=0\r\n"),
    ("max-age large", "Cache-Control: max-age=3600\r\n"),
    ("no-store", "Cache-Control: no-store\r\n"),
    ("only-if-cached", "Cache-Control: only-if-cached\r\n"),
    ("min-fresh", "Cache-Control: min-fresh=10\r\n"),
    (
        "min-fresh beyond the lifetime",
        "Cache-Control: min-fresh=3600\r\n",
    ),
    ("max-stale", "Cache-Control: max-stale\r\n"),
    ("malformed directive", "Cache-Control: max-age=\"x\r\n"),
    ("pragma no-cache", "Pragma: no-cache\r\n"),
    (
        "pragma and cache-control",
        "Pragma: no-cache\r\nCache-Control: max-age=60\r\n",
    ),
    ("authorization", "Authorization: Bearer secret\r\n"),
    ("cookie", "Cookie: a=1\r\n"),
    ("accept-encoding", "Accept-Encoding: gzip, br\r\n"),
    ("accept-language", "Accept-Language: fr\r\n"),
    ("accept-language other", "Accept-Language: de, en;q=0.5\r\n"),
    ("content-length zero", "Content-Length: 0\r\n"),
    ("request id", "X-Request-Id: abc-123\r\n"),
];

pub fn cache_requests(base: &str) -> Vec<Case> {
    let mut cases = Vec::new();
    for (name, headers) in FIELDS {
        add(&mut cases, name, get(base, headers));
    }
    for (name, headers) in [
        ("head", ""),
        ("head if-none-match", "If-None-Match: \"v1\"\r\n"),
        ("head no-cache", "Cache-Control: no-cache\r\n"),
        ("head close", "Connection: close\r\n"),
    ] {
        add(
            &mut cases,
            name,
            format!("HEAD {base} HTTP/1.1\r\n{HOST}{headers}\r\n"),
        );
    }
    add(
        &mut cases,
        "other host",
        format!("GET {base} HTTP/1.1\r\nHost: other.test\r\n\r\n"),
    );
    add(
        &mut cases,
        "host with port",
        format!("GET {base} HTTP/1.1\r\nHost: example.test:8080\r\n\r\n"),
    );
    add(
        &mut cases,
        "host in capitals",
        format!("GET {base} HTTP/1.1\r\nHost: EXAMPLE.test\r\n\r\n"),
    );
    add(&mut cases, "other query", get(&format!("{base}?x=1"), ""));
    add(
        &mut cases,
        "same query again",
        get(&format!("{base}?x=1"), ""),
    );
    add(&mut cases, "other path", get(&format!("{base}/more"), ""));
    cases.push(Case::many(
        "a hit then a condition on one connection",
        &[
            get(base, "").as_bytes(),
            get(base, "If-None-Match: \"v1\"\r\n").as_bytes(),
            get(base, "").as_bytes(),
        ],
    ));
    cases.push(Case::many(
        "two requests in one write",
        &[format!("{}{}", get(base, ""), get(base, "")).as_bytes()],
    ));
    cases.push(Case::many(
        "a head split over two writes",
        &[
            format!("GET {base} HTTP/1.1\r\nHost: exam").as_bytes(),
            b"ple.test\r\n\r\n",
        ],
    ));
    cases
}
