//! Requests of the differential tests: browser and tool shaped heads, hop-by-hop fields and `Connection` tokens,
//! smuggling shaped framing, request targets of every form, `Expect`, upgrades, pipelining and split writes.

pub struct Case {
    pub name: String,
    writes: Vec<Vec<u8>>,
}

impl Case {
    fn one(name: impl Into<String>, request: impl Into<Vec<u8>>) -> Self {
        Self {
            name: name.into(),
            writes: vec![request.into()],
        }
    }

    fn many(name: impl Into<String>, writes: &[&[u8]]) -> Self {
        Self {
            name: name.into(),
            writes: writes.iter().map(|write| write.to_vec()).collect(),
        }
    }

    pub fn script(&self) -> Vec<&[u8]> {
        self.writes.iter().map(Vec::as_slice).collect()
    }
}

const HOST: &str = "Host: example.test\r\n";

fn add(cases: &mut Vec<Case>, name: &str, request: impl Into<Vec<u8>>) {
    cases.push(Case::one(name, request));
}

fn get(path: &str, headers: &str) -> String {
    format!("GET {path} HTTP/1.1\r\n{HOST}{headers}\r\n")
}

pub fn requests(base: &str) -> Vec<Case> {
    let mut cases = Vec::new();
    add(&mut cases, "plain", get(base, ""));
    add(&mut cases, "close", get(base, "Connection: close\r\n"));
    add(
        &mut cases,
        "keep-alive",
        get(base, "Connection: keep-alive\r\n"),
    );
    add(
        &mut cases,
        "keep-alive mixed case",
        get(base, "connection: Keep-Alive\r\n"),
    );
    add(
        &mut cases,
        "close mixed case",
        get(base, "CONNECTION: CLOSE\r\n"),
    );
    add(&mut cases, "query", get(&format!("{base}?a=1&b=2"), ""));
    add(&mut cases, "empty query", get(&format!("{base}?"), ""));
    add(
        &mut cases,
        "percent path",
        get(&format!("{base}/a%20b/%7Ec"), ""),
    );
    add(&mut cases, "double slash", get(&format!("{base}//x"), ""));
    add(
        &mut cases,
        "dot segments",
        get(&format!("{base}/a/./b/../c"), ""),
    );
    add(
        &mut cases,
        "encoded dot segments",
        get(&format!("{base}/a/%2e%2e/b"), ""),
    );
    add(
        &mut cases,
        "encoded slash",
        get(&format!("{base}/a%2fb"), ""),
    );
    add(&mut cases, "fragment", get(&format!("{base}#frag"), ""));
    add(
        &mut cases,
        "space in target",
        format!("GET {base}/a b HTTP/1.1\r\n{HOST}\r\n"),
    );
    add(
        &mut cases,
        "non ascii target",
        format!("GET {base}/é HTTP/1.1\r\n{HOST}\r\n"),
    );
    add(
        &mut cases,
        "long target",
        get(&format!("{base}/{}", "a".repeat(6000)), ""),
    );
    add(
        &mut cases,
        "head",
        format!("HEAD {base} HTTP/1.1\r\n{HOST}\r\n"),
    );
    add(
        &mut cases,
        "delete",
        format!("DELETE {base} HTTP/1.1\r\n{HOST}\r\n"),
    );
    add(
        &mut cases,
        "options",
        format!("OPTIONS {base} HTTP/1.1\r\n{HOST}\r\n"),
    );
    add(
        &mut cases,
        "custom method",
        format!("PROPFIND {base} HTTP/1.1\r\n{HOST}\r\n"),
    );
    add(
        &mut cases,
        "lowercase method",
        format!("get {base} HTTP/1.1\r\n{HOST}\r\n"),
    );
    add(
        &mut cases,
        "post zero length",
        format!("POST {base} HTTP/1.1\r\n{HOST}Content-Length: 0\r\n\r\n"),
    );
    add(
        &mut cases,
        "post body",
        format!("POST {base} HTTP/1.1\r\n{HOST}Content-Length: 5\r\n\r\nhello"),
    );
    add(
        &mut cases,
        "get with body",
        format!("GET {base} HTTP/1.1\r\n{HOST}Content-Length: 3\r\n\r\nabc"),
    );
    add(
        &mut cases,
        "chunked",
        format!(
            "POST {base} HTTP/1.1\r\n{HOST}Transfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n0\r\n\r\n"
        ),
    );
    add(
        &mut cases,
        "chunked and length",
        format!(
            "POST {base} HTTP/1.1\r\n{HOST}Content-Length: 3\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n0\r\n\r\n"
        ),
    );
    add(
        &mut cases,
        "te not chunked",
        format!("POST {base} HTTP/1.1\r\n{HOST}Transfer-Encoding: gzip\r\n\r\n"),
    );
    add(
        &mut cases,
        "duplicate length",
        format!("POST {base} HTTP/1.1\r\n{HOST}Content-Length: 2\r\nContent-Length: 2\r\n\r\nab"),
    );
    add(
        &mut cases,
        "conflicting length",
        format!("POST {base} HTTP/1.1\r\n{HOST}Content-Length: 2\r\nContent-Length: 3\r\n\r\nab"),
    );
    add(
        &mut cases,
        "comma length",
        format!("POST {base} HTTP/1.1\r\n{HOST}Content-Length: 2, 2\r\n\r\nab"),
    );
    add(
        &mut cases,
        "signed length",
        format!("POST {base} HTTP/1.1\r\n{HOST}Content-Length: +2\r\n\r\nab"),
    );
    add(
        &mut cases,
        "huge length",
        format!("POST {base} HTTP/1.1\r\n{HOST}Content-Length: 99999999999999999999\r\n\r\n"),
    );
    add(
        &mut cases,
        "length zero padded",
        format!("POST {base} HTTP/1.1\r\n{HOST}Content-Length: 000\r\n\r\n"),
    );
    add(
        &mut cases,
        "length space",
        format!("POST {base} HTTP/1.1\r\n{HOST}Content-Length:  0 \r\n\r\n"),
    );
    add(
        &mut cases,
        "expect continue",
        format!(
            "POST {base} HTTP/1.1\r\n{HOST}Content-Length: 3\r\nExpect: 100-continue\r\n\r\nabc"
        ),
    );
    add(
        &mut cases,
        "expect on get",
        get(base, "Expect: 100-continue\r\n"),
    );
    add(
        &mut cases,
        "websocket upgrade",
        get(
            base,
            "Connection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n",
        ),
    );
    add(
        &mut cases,
        "upgrade without connection",
        get(base, "Upgrade: websocket\r\n"),
    );
    add(
        &mut cases,
        "connection upgrade without upgrade",
        get(base, "Connection: Upgrade\r\n"),
    );
    add(
        &mut cases,
        "h2c upgrade",
        get(
            base,
            "Connection: Upgrade, HTTP2-Settings\r\nUpgrade: h2c\r\nHTTP2-Settings: AAMAAABkAAQAAP__\r\n",
        ),
    );
    add(
        &mut cases,
        "keep-alive field",
        get(base, "Keep-Alive: timeout=5\r\n"),
    );
    add(
        &mut cases,
        "proxy connection",
        get(base, "Proxy-Connection: keep-alive\r\n"),
    );
    add(
        &mut cases,
        "proxy authorization",
        get(base, "Proxy-Authorization: Basic YWxpY2U6cHc=\r\n"),
    );
    add(
        &mut cases,
        "proxy authenticate",
        get(base, "Proxy-Authenticate: Basic\r\n"),
    );
    add(&mut cases, "te trailers", get(base, "TE: trailers\r\n"));
    add(
        &mut cases,
        "te gzip",
        get(base, "TE: gzip, trailers\r\nConnection: TE\r\n"),
    );
    add(&mut cases, "trailer field", get(base, "Trailer: X-Sum\r\n"));
    add(
        &mut cases,
        "nominated header",
        get(
            base,
            "Connection: keep-alive, X-Nominated\r\nX-Nominated: 1\r\nX-Other: 2\r\n",
        ),
    );
    add(
        &mut cases,
        "nominated forwarded header",
        get(
            base,
            "Connection: X-Forwarded-For\r\nX-Forwarded-For: 192.0.2.19\r\n",
        ),
    );
    add(
        &mut cases,
        "nominated close",
        get(base, "Connection: close, X-Nominated\r\nX-Nominated: 1\r\n"),
    );
    add(
        &mut cases,
        "nominated length",
        get(base, "Connection: Content-Length\r\n"),
    );
    add(
        &mut cases,
        "nominated host",
        get(base, "Connection: Host\r\n"),
    );
    add(
        &mut cases,
        "nominated transfer encoding",
        get(base, "Connection: Transfer-Encoding\r\n"),
    );
    add(
        &mut cases,
        "nominated two words",
        get(base, "Connection: two words\r\n"),
    );
    add(
        &mut cases,
        "nominated empty items",
        get(base, "Connection: , close,\r\n"),
    );
    add(
        &mut cases,
        "two connection fields",
        get(base, "Connection: keep-alive\r\nConnection: close\r\n"),
    );
    add(
        &mut cases,
        "hop by hop removal order",
        get(
            base,
            "A: 1\r\nKeep-Alive: 1\r\nB: 2\r\nTE: trailers\r\nC: 3\r\nConnection: keep-alive\r\nD: 4\r\n",
        ),
    );
    add(
        &mut cases,
        "browser head",
        format!(
            "GET {base} HTTP/1.1\r\nHost: example.test\r\nUser-Agent: Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36\r\nAccept: text/html,application/xhtml+xml\r\nAccept-Language: en-US,en;q=0.9\r\nAccept-Encoding: gzip, deflate, br\r\nCache-Control: max-age=0\r\nCookie: a=1; b=2; c=3\r\nReferer: https://example.test/\r\nUpgrade-Insecure-Requests: 1\r\nSec-Fetch-Dest: document\r\nSec-Fetch-Mode: navigate\r\nConnection: keep-alive\r\n\r\n"
        ),
    );
    add(
        &mut cases,
        "curl head",
        format!(
            "GET {base} HTTP/1.1\r\nHost: example.test\r\nUser-Agent: curl/8.0\r\nAccept: */*\r\n\r\n"
        ),
    );
    add(
        &mut cases,
        "header case",
        get(
            base,
            "x-lower: 1\r\nX-UPPER: 2\r\nX-Mixed-Case: 3\r\nContent-TYPE: text/plain\r\nACCEPT: */*\r\n",
        ),
    );
    add(
        &mut cases,
        "duplicate adjacent",
        get(base, "X-Dup: a\r\nX-Dup: b\r\n"),
    );
    add(
        &mut cases,
        "duplicate apart",
        get(
            base,
            "X-Dup: a\r\nX-Other: 1\r\nX-Dup: b\r\nX-Third: 3\r\nx-dup: c\r\n",
        ),
    );
    add(
        &mut cases,
        "duplicate accept",
        get(base, "Accept: a\r\nX: 1\r\nAccept: b\r\n"),
    );
    add(
        &mut cases,
        "empty value",
        get(base, "X-Empty:\r\nX-Space: \r\n"),
    );
    add(
        &mut cases,
        "value whitespace",
        get(base, "X-Pad:   padded value \t\r\n"),
    );
    add(&mut cases, "value tab inside", get(base, "X-Tab: a\tb\r\n"));
    add(
        &mut cases,
        "value with colon",
        get(base, "X-Colon: a:b:c\r\n"),
    );
    add(
        &mut cases,
        "high byte value",
        [
            get(base, "X-Hi: ").trim_end().as_bytes(),
            &[0xff, 0xfe],
            b"\r\n\r\n",
        ]
        .concat(),
    );
    add(&mut cases, "obs fold", get(base, "X-Fold: a\r\n b\r\n"));
    add(
        &mut cases,
        "space before colon",
        get(base, "X-Space : 1\r\n"),
    );
    add(&mut cases, "empty name", get(base, ": 1\r\n"));
    add(&mut cases, "no colon", get(base, "X-Broken\r\n"));
    add(&mut cases, "underscore name", get(base, "X_Under: 1\r\n"));
    add(&mut cases, "bad name char", get(base, "X@Bad: 1\r\n"));
    add(
        &mut cases,
        "many headers",
        get(
            base,
            &(0..70)
                .map(|i| format!("X-H{i}: v{i}\r\n"))
                .collect::<String>(),
        ),
    );
    add(
        &mut cases,
        "sixty four headers",
        get(
            base,
            &(0..63)
                .map(|i| format!("X-H{i}: v{i}\r\n"))
                .collect::<String>(),
        ),
    );
    add(
        &mut cases,
        "two hundred sixty headers",
        get(
            base,
            &(0..260)
                .map(|i| format!("X-H{i}: v{i}\r\n"))
                .collect::<String>(),
        ),
    );
    add(
        &mut cases,
        "large value",
        get(base, &format!("X-Big: {}\r\n", "v".repeat(7000))),
    );
    add(
        &mut cases,
        "huge head",
        get(base, &format!("X-Big: {}\r\n", "v".repeat(70_000))),
    );
    add(
        &mut cases,
        "request id",
        get(base, "X-Request-Id: abc-123\r\n"),
    );
    add(
        &mut cases,
        "request id invalid",
        get(base, "X-Request-Id: bad id!\r\n"),
    );
    add(
        &mut cases,
        "request id twice",
        get(base, "X-Request-Id: a1\r\nX-Request-Id: b2\r\n"),
    );
    add(
        &mut cases,
        "request id empty",
        get(base, "X-Request-Id:\r\n"),
    );
    add(
        &mut cases,
        "request id long",
        get(base, &format!("X-Request-Id: {}\r\n", "a".repeat(65))),
    );
    add(
        &mut cases,
        "x-forwarded-for client",
        get(base, "X-Forwarded-For: 192.0.2.201\r\nX-Real-IP: 192.0.2.201\r\n"),
    );
    add(
        &mut cases,
        "authorization",
        get(base, "Authorization: Bearer abc\r\n"),
    );
    add(
        &mut cases,
        "host with port",
        format!("GET {base} HTTP/1.1\r\nHost: example.test:8080\r\n\r\n"),
    );
    add(
        &mut cases,
        "host uppercase",
        format!("GET {base} HTTP/1.1\r\nHost: EXAMPLE.TEST\r\n\r\n"),
    );
    add(
        &mut cases,
        "host ipv6",
        format!("GET {base} HTTP/1.1\r\nHost: [::1]:8080\r\n\r\n"),
    );
    add(
        &mut cases,
        "host userinfo",
        format!("GET {base} HTTP/1.1\r\nHost: alice@example.test\r\n\r\n"),
    );
    add(
        &mut cases,
        "host empty",
        format!("GET {base} HTTP/1.1\r\nHost:\r\n\r\n"),
    );
    add(
        &mut cases,
        "host space",
        format!("GET {base} HTTP/1.1\r\nHost: exa mple\r\n\r\n"),
    );
    add(
        &mut cases,
        "host trailing dot",
        format!("GET {base} HTTP/1.1\r\nHost: example.test.\r\n\r\n"),
    );
    add(
        &mut cases,
        "host missing",
        format!("GET {base} HTTP/1.1\r\nAccept: */*\r\n\r\n"),
    );
    add(
        &mut cases,
        "host twice",
        format!("GET {base} HTTP/1.1\r\nHost: a.test\r\nHost: b.test\r\n\r\n"),
    );
    add(
        &mut cases,
        "host twice same",
        format!("GET {base} HTTP/1.1\r\nHost: a.test\r\nHost: a.test\r\n\r\n"),
    );
    add(
        &mut cases,
        "host lowercase name",
        format!("GET {base} HTTP/1.1\r\nhost: example.test\r\n\r\n"),
    );
    add(
        &mut cases,
        "absolute form",
        format!("GET http://example.test{base} HTTP/1.1\r\nHost: example.test\r\n\r\n"),
    );
    add(
        &mut cases,
        "absolute form other host",
        format!("GET http://other.test{base} HTTP/1.1\r\nHost: example.test\r\n\r\n"),
    );
    add(
        &mut cases,
        "absolute form no host",
        format!("GET http://example.test{base} HTTP/1.1\r\nAccept: */*\r\n\r\n"),
    );
    add(
        &mut cases,
        "absolute form https",
        format!("GET https://example.test{base} HTTP/1.1\r\nHost: example.test\r\n\r\n"),
    );
    add(
        &mut cases,
        "absolute form userinfo",
        format!("GET http://a@example.test{base} HTTP/1.1\r\nHost: example.test\r\n\r\n"),
    );
    add(
        &mut cases,
        "asterisk form",
        "OPTIONS * HTTP/1.1\r\nHost: example.test\r\n\r\n".to_owned(),
    );
    add(
        &mut cases,
        "authority form",
        "CONNECT example.test:443 HTTP/1.1\r\nHost: example.test:443\r\n\r\n".to_owned(),
    );
    add(
        &mut cases,
        "http 1.0",
        format!("GET {base} HTTP/1.0\r\n\r\n"),
    );
    add(
        &mut cases,
        "http 1.0 host",
        format!("GET {base} HTTP/1.0\r\nHost: example.test\r\n\r\n"),
    );
    add(
        &mut cases,
        "http 1.0 keep-alive",
        format!("GET {base} HTTP/1.0\r\nHost: example.test\r\nConnection: keep-alive\r\n\r\n"),
    );
    add(
        &mut cases,
        "http 1.0 transfer encoding",
        format!(
            "POST {base} HTTP/1.0\r\nHost: example.test\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n"
        ),
    );
    add(&mut cases, "http 0.9", format!("GET {base}\r\n"));
    add(
        &mut cases,
        "http 2.0 version",
        format!("GET {base} HTTP/2.0\r\nHost: example.test\r\n\r\n"),
    );
    add(
        &mut cases,
        "http 1.2 version",
        format!("GET {base} HTTP/1.2\r\nHost: example.test\r\n\r\n"),
    );
    add(
        &mut cases,
        "bare lf",
        format!("GET {base} HTTP/1.1\nHost: example.test\nX-A: 1\n\n"),
    );
    add(
        &mut cases,
        "bare lf mixed",
        format!("GET {base} HTTP/1.1\r\nHost: example.test\nX-A: 1\r\n\r\n"),
    );
    add(
        &mut cases,
        "bare cr",
        format!("GET {base} HTTP/1.1\r\nHost: example.test\rX-A: 1\r\n\r\n"),
    );
    add(&mut cases, "cr in value", get(base, "X-A: a\rb\r\n"));
    add(
        &mut cases,
        "nul in value",
        [
            get(base, "X-A: a").trim_end().as_bytes(),
            &[0],
            b"b\r\n\r\n",
        ]
        .concat(),
    );
    add(&mut cases, "del in value", get(base, "X-A: a\x7fb\r\n"));
    add(
        &mut cases,
        "leading crlf",
        format!("\r\nGET {base} HTTP/1.1\r\n{HOST}\r\n"),
    );
    add(
        &mut cases,
        "trailing space request line",
        format!("GET {base} HTTP/1.1 \r\n{HOST}\r\n"),
    );
    add(
        &mut cases,
        "double space request line",
        format!("GET  {base} HTTP/1.1\r\n{HOST}\r\n"),
    );
    add(
        &mut cases,
        "tab request line",
        format!("GET\t{base} HTTP/1.1\r\n{HOST}\r\n"),
    );
    add(
        &mut cases,
        "garbage",
        b"\x16\x03\x01\x02\x00\x01\x00\x01\xfc\x03\x03".to_vec(),
    );
    add(&mut cases, "empty line only", "\r\n\r\n".to_owned());
    cases.extend(pipelining(base));
    cases.extend(split_writes(base));
    cases
}

fn pipelining(base: &str) -> Vec<Case> {
    let one = get(base, "");
    let two = get(&format!("{base}2"), "");
    vec![
        Case::one("pipelined pair", format!("{one}{two}")),
        Case::one("pipelined triple", format!("{one}{two}{one}")),
        Case::one(
            "pipelined with body",
            format!("POST {base} HTTP/1.1\r\n{HOST}Content-Length: 3\r\n\r\nabc{one}"),
        ),
        Case::one("pipelined garbage after", format!("{one}garbage")),
        Case::one("trailing crlf after head", format!("{one}\r\n")),
    ]
}

fn split_writes(base: &str) -> Vec<Case> {
    let request = get(base, "X-A: 1\r\n");
    let bytes = request.as_bytes();
    vec![
        Case::many("split in request line", &[&bytes[..5], &bytes[5..]]),
        Case::many("split in header", &[&bytes[..30], &bytes[30..]]),
        Case::many(
            "split before blank line",
            &[&bytes[..bytes.len() - 2], &bytes[bytes.len() - 2..]],
        ),
        Case::many(
            "split in last crlf",
            &[&bytes[..bytes.len() - 1], &bytes[bytes.len() - 1..]],
        ),
        Case::many("byte by byte", &bytes.chunks(1).collect::<Vec<_>>()),
        Case::many("two requests two writes", &[bytes, bytes]),
        Case::many(
            "request then close",
            &[
                bytes,
                b"GET /x HTTP/1.1\r\nHost: example.test\r\nConnection: close\r\n\r\n",
            ],
        ),
    ]
}

/// Connections that alternate between what the lane serves and what only the classic path serves.
pub fn sequences() -> Vec<Case> {
    let lane = get("/r", "");
    let fixed = get("/fixed", "");
    let close = get("/r", "Connection: close\r\n");
    let body = format!("POST /r HTTP/1.1\r\n{HOST}Content-Length: 3\r\n\r\nabc");
    let upgrade = get("/r", "Connection: Upgrade\r\nUpgrade: websocket\r\n");
    let absolute = format!("GET http://example.test/r HTTP/1.1\r\nHost: example.test\r\n\r\n");
    vec![
        Case::many(
            "lane lane lane",
            &[lane.as_bytes(), lane.as_bytes(), lane.as_bytes()],
        ),
        Case::many(
            "lane classic lane",
            &[lane.as_bytes(), body.as_bytes(), lane.as_bytes()],
        ),
        Case::many(
            "classic lane classic",
            &[body.as_bytes(), lane.as_bytes(), body.as_bytes()],
        ),
        Case::many(
            "fixed proxied fixed",
            &[fixed.as_bytes(), lane.as_bytes(), fixed.as_bytes()],
        ),
        Case::many(
            "fixed then body then fixed",
            &[fixed.as_bytes(), body.as_bytes(), fixed.as_bytes()],
        ),
        Case::many(
            "lane then close",
            &[lane.as_bytes(), close.as_bytes(), lane.as_bytes()],
        ),
        Case::many(
            "fixed then close",
            &[
                fixed.as_bytes(),
                get("/fixed", "Connection: close\r\n").as_bytes(),
                fixed.as_bytes(),
            ],
        ),
        Case::many("lane then upgrade", &[lane.as_bytes(), upgrade.as_bytes()]),
        Case::many(
            "absolute between",
            &[lane.as_bytes(), absolute.as_bytes(), lane.as_bytes()],
        ),
        Case::many(
            "refused between",
            &[
                lane.as_bytes(),
                b"GET /r HTTP/1.1\r\n\r\nGET".as_slice(),
                lane.as_bytes(),
            ],
        ),
        Case::many("many keep-alive", &vec![lane.as_bytes(); 12]),
    ]
}
