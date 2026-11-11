//! Requests of the production shapes: header rules read forwarded-for chains, names and hosts in every spelling;
//! bodies of every framing, size and ending.

use super::{Case, HOST, get, numbered_headers};

/// Requests that header rules read: forwarded-for chains, names in every case, hosts in every spelling, headers a rule
/// removes or replaces and values too large for a rule.
pub fn forwarded(base: &str) -> Vec<Case> {
    let mut cases = Vec::new();
    let mut add = |name: &str, headers: &str| cases.push(Case::one(name, get(base, headers)));
    add("no forwarded headers", "");
    add("one forwarded-for", "X-Forwarded-For: 203.0.113.5\r\n");
    add(
        "forwarded-for chain",
        "X-Forwarded-For: 203.0.113.5, 198.51.100.7\r\n",
    );
    add(
        "two forwarded-for fields",
        "X-Forwarded-For: 203.0.113.5\r\nX-Forwarded-For: 198.51.100.7\r\n",
    );
    add(
        "forwarded-for in capitals",
        "X-FORWARDED-FOR: 203.0.113.5\r\n",
    );
    add("empty forwarded-for", "X-Forwarded-For:\r\n");
    add(
        "forwarded-for too large for the rule",
        &format!("X-Forwarded-For: {}\r\n", "1".repeat(300)),
    );
    add(
        "forwarded-for that fills the rule",
        &format!("X-Forwarded-For: {}\r\n", "1".repeat(240)),
    );
    add(
        "forwarded-proto from the client",
        "X-Forwarded-Proto: https\r\n",
    );
    add("real ip from the client", "X-Real-IP: 192.0.2.99\r\n");
    add("a header a rule reads", "X-A: first\r\n");
    add(
        "a header a rule reads, twice",
        "X-A: first\r\nx-a: second\r\n",
    );
    add("a header a rule reads, empty", "X-A:\r\n");
    add(
        "a header a rule reads, too large",
        &format!("X-A: {}\r\n", "a".repeat(100)),
    );
    add("a header a rule removes", "X-Secret: s\r\n");
    add(
        "a header a rule removes, twice",
        "X-Secret: s\r\nx-secret: t\r\nX-B: 1\r\n",
    );
    add(
        "a header a rule removes and keeps order",
        "X-C: 1\r\nX-Secret: s\r\nX-D: 2\r\nX-E: 3\r\n",
    );
    add(
        "a header a rule sets, present",
        "X-Literal: client\r\nX-Literal: client2\r\n",
    );
    add("client request id", "X-Request-Id: client-id-1\r\n");
    add(
        "two request ids",
        "X-Request-Id: a1\r\nX-Request-Id: b2\r\n",
    );
    add("keep-alive", "Connection: keep-alive\r\n");
    add("close", "Connection: close\r\n");
    add("many headers", &numbered_headers(30));
    let hosts = [
        ("host in capitals", "EXAMPLE.test"),
        ("host with a port", "example.test:8080"),
        ("host with a trailing dot", "example.test."),
        ("host that is an address", "192.0.2.10"),
        ("host that is an address with a port", "192.0.2.10:81"),
        ("host that is an IPv6 address", "[2001:db8::1]"),
        (
            "host that is an IPv6 address with a port",
            "[2001:db8::1]:8080",
        ),
    ];
    for (name, host) in hosts {
        cases.push(Case::one(
            name,
            format!("GET {base} HTTP/1.1\r\nHost: {host}\r\n\r\n"),
        ));
    }
    cases.push(Case::one(
        "head",
        format!("HEAD {base} HTTP/1.1\r\n{HOST}X-Forwarded-For: 203.0.113.5\r\n\r\n"),
    ));
    cases.push(Case::many(
        "two requests",
        &[
            get(base, "X-A: 1\r\n").as_bytes(),
            get(base, "X-Forwarded-For: 203.0.113.5\r\n").as_bytes(),
        ],
    ));
    cases
}

fn post(base: &str, headers: &str, body: impl AsRef<[u8]>) -> Vec<u8> {
    let mut request = format!("POST {base} HTTP/1.1\r\n{HOST}{headers}\r\n").into_bytes();
    request.extend_from_slice(body.as_ref());
    request
}

fn sized(length: usize) -> Vec<u8> {
    (0..length)
        .map(|at| b'a' + u8::try_from(at % 23).expect("below 23"))
        .collect()
}

fn chunked(pieces: &[&[u8]]) -> Vec<u8> {
    let mut body = Vec::new();
    for piece in pieces {
        body.extend_from_slice(format!("{:x}\r\n", piece.len()).as_bytes());
        body.extend_from_slice(piece);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(b"0\r\n\r\n");
    body
}

/// A chunked body over the limit of the route: the origin has seen part of it when the client is refused, and how much
/// depends on the reads.
pub fn chunked_over_the_limit(base: &str) -> Case {
    let head = post(base, "Transfer-Encoding: chunked\r\n", "");
    let big = sized(40_000);
    Case::writes(
        "chunked over the limit of the route",
        vec![head, chunked(&[&big, &big])],
    )
}

/// Requests with bodies: every framing, sizes around the limits of the route, bodies that arrive in pieces, bodies that
/// end badly and requests that follow a body.
#[allow(clippy::too_many_lines)]
pub fn bodies(base: &str) -> Vec<Case> {
    let length = |n: usize| format!("Content-Length: {n}\r\n");
    let te = "Transfer-Encoding: chunked\r\n";
    let mut cases = Vec::new();
    for size in [1, 5, 1024, 16 * 1024 - 1, 16 * 1024, 60_000, 65_536] {
        cases.push(Case::one(
            format!("post of {size} bytes"),
            post(base, &length(size), sized(size)),
        ));
    }
    cases.push(Case::one(
        "post over the limit of the route",
        post(base, &length(65_537), sized(65_537)),
    ));
    cases.push(Case::one(
        "post of a length that is never sent",
        post(base, &length(100), sized(10)),
    ));
    cases.push(Case::one(
        "put",
        format!("PUT {base} HTTP/1.1\r\n{HOST}Content-Length: 4\r\n\r\ndata"),
    ));
    cases.push(Case::one(
        "get with a body",
        format!("GET {base} HTTP/1.1\r\n{HOST}Content-Length: 3\r\n\r\nabc"),
    ));
    cases.push(Case::one(
        "head with a body",
        format!("HEAD {base} HTTP/1.1\r\n{HOST}Content-Length: 3\r\n\r\nabc"),
    ));
    cases.push(Case::one(
        "post with a body and close",
        post(base, "Content-Length: 3\r\nConnection: close\r\n", "abc"),
    ));
    cases.push(Case::one(
        "post with a body and keep-alive",
        post(
            base,
            "Content-Length: 3\r\nConnection: keep-alive\r\n",
            "abc",
        ),
    ));
    cases.push(Case::one(
        "post with rules to apply",
        post(
            base,
            "Content-Length: 3\r\nX-Forwarded-For: 203.0.113.5\r\nX-A: 1\r\n",
            "abc",
        ),
    ));
    cases.push(Case::one(
        "chunked, one chunk",
        post(base, te, chunked(&[b"hello"])),
    ));
    cases.push(Case::one(
        "chunked, several chunks",
        post(base, te, chunked(&[b"hello", b" ", b"world"])),
    ));
    cases.push(Case::one(
        "chunked, large chunk",
        post(base, te, chunked(&[&sized(40_000)])),
    ));
    cases.push(Case::one("chunked, empty", post(base, te, "0\r\n\r\n")));
    cases.push(Case::one(
        "chunked, upper case size",
        post(base, te, "A\r\n0123456789\r\n0\r\n\r\n"),
    ));
    cases.push(Case::one(
        "chunked with an extension",
        post(base, te, "5;name=value\r\nhello\r\n0\r\n\r\n"),
    ));
    cases.push(Case::one(
        "chunked with a trailer",
        post(base, te, "5\r\nhello\r\n0\r\nX-Trailer: 1\r\n\r\n"),
    ));
    cases.push(Case::one(
        "chunked with a bad size",
        post(base, te, "zz\r\nhello\r\n0\r\n\r\n"),
    ));
    cases.push(Case::one(
        "chunked with a missing terminator",
        post(base, te, "5\r\nhello\r\n"),
    ));
    cases.push(Case::one(
        "chunked, then a request in the same write",
        [
            post(base, te, chunked(&[b"hello"])),
            get(base, "").into_bytes(),
        ]
        .concat(),
    ));
    cases.push(Case::one(
        "length, then a request in the same write",
        [post(base, &length(5), "hello"), get(base, "").into_bytes()].concat(),
    ));
    let bytes = |text: &[u8]| text.to_vec();
    let head = post(base, &length(10), "");
    cases.push(Case::writes(
        "body after the head",
        vec![head.clone(), bytes(b"0123456789")],
    ));
    cases.push(Case::writes(
        "body in two pieces",
        vec![post(base, &length(10), "01234"), bytes(b"56789")],
    ));
    let chunked_head = post(base, te, "");
    cases.push(Case::writes(
        "chunks in separate writes",
        vec![
            chunked_head.clone(),
            bytes(b"5\r\nhello\r\n"),
            bytes(b"6\r\n world\r\n"),
            bytes(b"0\r\n\r\n"),
        ],
    ));
    cases.push(Case::writes(
        "chunk split in its size line",
        vec![
            chunked_head,
            bytes(b"5"),
            bytes(b"\r\nhello\r\n0"),
            bytes(b"\r\n\r\n"),
        ],
    ));
    let first = post(base, &length(3), "abc");
    cases.push(Case::writes(
        "two posts on one connection",
        vec![first.clone(), first.clone()],
    ));
    cases.push(Case::writes(
        "post, get, post",
        vec![first.clone(), get(base, "").into_bytes(), first],
    ));
    let chunked_post = post(base, te, chunked(&[b"abc"]));
    cases.push(Case::writes(
        "chunked post, get, chunked post",
        vec![
            chunked_post.clone(),
            get(base, "").into_bytes(),
            chunked_post,
        ],
    ));
    cases.push(Case::one(
        "expect 100-continue",
        post(base, "Content-Length: 3\r\nExpect: 100-continue\r\n", "abc"),
    ));
    cases.push(Case::writes(
        "expect 100-continue, body after the head",
        vec![
            post(base, "Content-Length: 3\r\nExpect: 100-continue\r\n", ""),
            bytes(b"abc"),
        ],
    ));
    cases.push(Case::one(
        "chunked and a length",
        post(
            base,
            "Content-Length: 3\r\nTransfer-Encoding: chunked\r\n",
            chunked(&[b"abc"]),
        ),
    ));
    cases.push(Case::one(
        "chunked in capitals",
        post(base, "TRANSFER-ENCODING: Chunked\r\n", chunked(&[b"abc"])),
    ));
    cases
}
