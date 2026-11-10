//! Hand-written request heads with the verdict the scanner must give. `\n` in a text is sent as CRLF.

use oxiroute_h1_head::MissReason as M;

#[derive(Debug, Clone, Copy)]
pub enum Expect {
    Head,
    Miss(M),
}

pub type Case = (&'static str, Vec<u8>, Expect);

pub fn crlf(text: &str) -> Vec<u8> {
    text.replace('\n', "\r\n").into_bytes()
}

fn ok(name: &'static str, text: &str) -> Case {
    (name, crlf(text), Expect::Head)
}

fn miss(name: &'static str, text: &str, reason: M) -> Case {
    (name, crlf(text), Expect::Miss(reason))
}

fn raw(name: &'static str, bytes: &[u8], expect: Expect) -> Case {
    (name, bytes.to_vec(), expect)
}

fn post(headers: &str) -> String {
    format!("POST /u HTTP/1.1\nHost: a\n{headers}\n\n")
}

fn get(headers: &str) -> String {
    format!("GET / HTTP/1.1\nHost: a\n{headers}\n\n")
}

fn numbered(n: usize) -> String {
    (0..n).fold(String::new(), |all, i| all + &format!("X-H{i}: v{i}\n"))
}

const BROWSER: &str = "GET /static/app.js?v=3 HTTP/1.1\nHost: www.example.com\nConnection: keep-alive\n\
User-Agent: Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36\n\
Accept: text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8\n\
Accept-Language: en-US,en;q=0.9\nAccept-Encoding: gzip, deflate, br\nCache-Control: max-age=0\n\
Cookie: session=0123456789abcdef; theme=dark; lang=en\nReferer: https://www.example.com/index.html\n\
Sec-Fetch-Dest: document\nSec-Fetch-Mode: navigate\nUpgrade-Insecure-Requests: 1\n\n";

#[rustfmt::skip]
pub fn framing() -> Vec<Case> {
    vec![
        ok("post-length", "POST /upload HTTP/1.1\nHost: a\nContent-Length: 5\n\nhello"),
        ok("post-zero-length", &post("Content-Length: 0")),
        ok("length-leading-zeros", &post("Content-Length: 007")),
        ok("length-18-digits", &post("Content-Length: 999999999999999999")),
        miss("length-19-digits", &post("Content-Length: 9999999999999999999"), M::ContentLength),
        miss("length-signed", &post("Content-Length: +5"), M::ContentLength),
        miss("length-negative", &post("Content-Length: -1"), M::ContentLength),
        miss("length-comma", &post("Content-Length: 5, 5"), M::ContentLength),
        miss("length-duplicate", &post("Content-Length: 5\nContent-Length: 5"), M::ContentLength),
        miss("length-duplicate-differs", &post("Content-Length: 5\ncontent-length: 6"), M::ContentLength),
        miss("length-empty", &post("Content-Length:"), M::ContentLength),
        miss("length-hex", &post("Content-Length: 0x10"), M::ContentLength),
        miss("length-inner-space", &post("Content-Length: 1 2"), M::ContentLength),
        ok("length-padded", &post("Content-Length:  12 \t")),
        ok("chunked", &post("Transfer-Encoding: chunked")),
        ok("chunked-upper", &post("Transfer-Encoding: Chunked")),
        ok("gzip-chunked", &post("Transfer-Encoding: gzip, chunked")),
        miss("chunked-gzip", &post("Transfer-Encoding: chunked, gzip"), M::TransferEncoding),
        ok("gzip-then-chunked-headers", &post("Transfer-Encoding: gzip\nTransfer-Encoding: chunked")),
        miss("chunked-then-gzip-headers", &post("Transfer-Encoding: chunked\nTransfer-Encoding: gzip"), M::TransferEncoding),
        miss("xchunked", &post("Transfer-Encoding: xchunked"), M::TransferEncoding),
        miss("chunked-trailing-comma", &post("Transfer-Encoding: chunked,"), M::TransferEncoding),
        ok("te-and-length", &post("Content-Length: 5\nTransfer-Encoding: chunked")),
        miss("te-and-dup-length", &post("Content-Length: 5\nContent-Length: 6\nTransfer-Encoding: chunked"), M::ContentLength),
        miss("te-empty", &post("Transfer-Encoding:"), M::TransferEncoding),
        miss("smuggle-space-before-colon", &post("Content-Length: 4\nTransfer-Encoding : chunked"), M::SpaceBeforeColon),
        ok("smuggle-te-tab", &post("Content-Length: 4\nTransfer-Encoding:\tchunked")),
        miss("smuggle-te-second-x", &post("Content-Length: 4\nTransfer-Encoding: chunked\nTransfer-Encoding: x"), M::TransferEncoding),
        miss("http10", "GET / HTTP/1.0\nHost: a\n\n", M::Version10),
        miss("http10-te", "POST / HTTP/1.0\nHost: a\nTransfer-Encoding: chunked\n\n", M::Version10),
        ok("pipelined", "GET /1 HTTP/1.1\nHost: a\n\nGET /2 HTTP/1.1\nHost: a\n\n"),
        ok("body-follows", "POST /1 HTTP/1.1\nHost: a\nContent-Length: 3\n\nabc"),
    ]
}

#[rustfmt::skip]
pub fn line_and_bytes() -> Vec<Case> {
    vec![
        raw("http2-preface", b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n", Expect::Miss(M::Target)),
        miss("http11x", "GET / HTTP/1.2\nHost: a\n\n", M::Version),
        miss("http-lower", "GET / http/1.1\nHost: a\n\n", M::Version),
        miss("version-trailing-space", "GET / HTTP/1.1 \nHost: a\n\n", M::Version),
        raw("all-bare-lf", b"GET / HTTP/1.1\nHost: a\nX: y\n\n", Expect::Miss(M::BareLf)),
        raw("bare-lf-head-end", b"GET / HTTP/1.1\r\nHost: a\n\n", Expect::Miss(M::BareLf)),
        raw("bare-lf-in-header", b"GET / HTTP/1.1\r\nHost: a\nX: y\r\n\r\n", Expect::Miss(M::BareLf)),
        raw("bare-cr-value", b"GET / HTTP/1.1\r\nHost: a\r\nX: y\rz\r\n\r\n", Expect::Miss(M::BareCr)),
        raw("bare-cr-line-end", b"GET / HTTP/1.1\r\nHost: a\rX: y\r\n\r\n", Expect::Miss(M::BareCr)),
        raw("bare-cr-head-end", b"GET / HTTP/1.1\r\nHost: a\r\n\rX", Expect::Miss(M::BareCr)),
        raw("bare-cr-request-line", b"GET / HTTP/1.1\rHost: a\r\n\r\n", Expect::Miss(M::BareCr)),
        miss("folding", "GET / HTTP/1.1\nHost: a\nX: y\n  z\n\n", M::Folding),
        miss("folding-tab", "GET / HTTP/1.1\nHost: a\nX: y\n\tz\n\n", M::Folding),
        miss("folding-first", "GET / HTTP/1.1\n Host: a\n\n", M::Folding),
        miss("space-before-colon", "GET / HTTP/1.1\nHost : a\n\n", M::SpaceBeforeColon),
        miss("tab-before-colon", "GET / HTTP/1.1\nHost\t: a\n\n", M::SpaceBeforeColon),
        raw("nul-value", b"GET / HTTP/1.1\r\nHost: a\r\nX: y\x00z\r\n\r\n", Expect::Miss(M::Control)),
        raw("del-value", b"GET / HTTP/1.1\r\nHost: a\r\nX: y\x7fz\r\n\r\n", Expect::Miss(M::Control)),
        raw("control-value", b"GET / HTTP/1.1\r\nHost: a\r\nX: y\x1fz\r\n\r\n", Expect::Miss(M::Control)),
        raw("tab-in-value", b"GET / HTTP/1.1\r\nHost: a\r\nX: y\tz\t\r\n\r\n", Expect::Head),
        raw("tabs-around-value", b"GET / HTTP/1.1\r\nHost:\t a \t\r\nX:\t\t\r\n\r\n", Expect::Head),
        raw("high-bytes-value", b"GET / HTTP/1.1\r\nHost: a\r\nX: caf\xc3\xa9 \xff\r\n\r\n", Expect::Head),
        raw("high-byte-name", b"GET / HTTP/1.1\r\nHost: a\r\nX\xc3: y\r\n\r\n", Expect::Miss(M::Name)),
        raw("high-byte-target", b"GET /caf\xc3\xa9 HTTP/1.1\r\nHost: a\r\n\r\n", Expect::Miss(M::Target)),
        raw("high-byte-method", b"G\xc3T / HTTP/1.1\r\nHost: a\r\n\r\n", Expect::Miss(M::Method)),
        raw("leading-lf", b"\nGET / HTTP/1.1\r\nHost: a\r\n\r\n", Expect::Miss(M::Method)),
        miss("leading-crlf", "\nGET / HTTP/1.1\nHost: a\n\n", M::Method),
        miss("empty-name", "GET / HTTP/1.1\nHost: a\n: y\n\n", M::Name),
        miss("name-with-space", "GET / HTTP/1.1\nHost: a\nX Y: y\n\n", M::SpaceBeforeColon),
        miss("name-with-slash", "GET / HTTP/1.1\nHost: a\nX/Y: y\n\n", M::Name),
        miss("name-no-colon", "GET / HTTP/1.1\nHost: a\nXY\n\n", M::Name),
        ok("name-255", &get(&format!("{}: y", "n".repeat(255)))),
        miss("name-256", &get(&format!("{}: y", "n".repeat(256))), M::NameTooLong),
        ok("underscore-name", &get("X_Real_Ip: 1")),
        miss("absolute-form", "GET http://example.com/a HTTP/1.1\nHost: example.com\n\n", M::Target),
        miss("asterisk-form", "OPTIONS * HTTP/1.1\nHost: a\n\n", M::Target),
        miss("authority-form", "CONNECT example.com:443 HTTP/1.1\nHost: example.com:443\n\n", M::Target),
        miss("empty-target", "GET  HTTP/1.1\nHost: a\n\n", M::Target),
        miss("double-space", "GET  /a HTTP/1.1\nHost: a\n\n", M::Target),
        miss("space-in-target", "GET /a b HTTP/1.1\nHost: a\n\n", M::Version),
        miss("fragment", "GET /a#b HTTP/1.1\nHost: a\n\n", M::Target),
        miss("quote-in-target", "GET /a\"b HTTP/1.1\nHost: a\n\n", M::Target),
        miss("backtick-in-query", "GET /a?b`c HTTP/1.1\nHost: a\n\n", M::Target),
        miss("lt-in-target", "GET /a<b HTTP/1.1\nHost: a\n\n", M::Target),
        ok("braces-in-target", "GET /a{b}?c={d} HTTP/1.1\nHost: a\n\n"),
        ok("empty-query", "GET /a? HTTP/1.1\nHost: a\n\n"),
        ok("query-only", "GET /? HTTP/1.1\nHost: a\n\n"),
        ok("dot-segments", "GET /a/../b/./c//d HTTP/1.1\nHost: a\n\n"),
        ok("percent-and-plus", "GET /a%2Fb%00?c=%zz+d HTTP/1.1\nHost: a\n\n"),
        ok("target-8k", &format!("GET /{} HTTP/1.1\nHost: a\n\n", "a".repeat(8191))),
        ok("target-65534", &format!("GET /{} HTTP/1.1\nHost: a\n\n", "a".repeat(65533))),
        miss("target-65535", &format!("GET /{} HTTP/1.1\nHost: a\n\n", "a".repeat(65534)), M::TargetTooLong),
        ok("lowercase-method", "get / HTTP/1.1\nHost: a\n\n"),
        ok("extension-method", "PROPFIND /dav HTTP/1.1\nHost: a\n\n"),
        ok("method-32", &format!("{} / HTTP/1.1\nHost: a\n\n", "M".repeat(32))),
        miss("method-33", &format!("{} / HTTP/1.1\nHost: a\n\n", "M".repeat(33)), M::MethodTooLong),
        miss("method-with-at", "G@T / HTTP/1.1\nHost: a\n\n", M::Method),
    ]
}

#[rustfmt::skip]
pub fn shapes() -> Vec<Case> {
    let mut c = vec![
        ok("minimal", "GET / HTTP/1.1\nHost: a\n\n"),
        ok("curl", "GET /get?x=1&y=%20z HTTP/1.1\nHost: 127.0.0.1:8080\nUser-Agent: curl/8.9.1\nAccept: */*\n\n"),
        ok("wget", "GET /f.tgz HTTP/1.1\nUser-Agent: Wget/1.24\nAccept: */*\nAccept-Encoding: identity\nHost: example.com\nConnection: Keep-Alive\n\n"),
        ok("browser", BROWSER),
        miss("no-host", "GET / HTTP/1.1\nAccept: */*\n\n", M::HostMissing),
        miss("two-hosts", "GET / HTTP/1.1\nHost: a\nhost: b\n\n", M::HostDuplicate),
        ok("empty-host", "GET / HTTP/1.1\nHost:\n\n"),
        ok("host-last", "GET / HTTP/1.1\nA: 1\nB: 2\nHOST: a\n\n"),
        ok("empty-values", &get("X:\nY: \nZ:  ")),
        ok("connection-close", &get("Connection: close")),
        ok("connection-list", &get("Connection: keep-alive, Upgrade\nUpgrade: websocket")),
        ok("connection-twice", &get("Connection: close\nConnection: keep-alive")),
        ok("hop-by-hop", &get("Keep-Alive: 5\nProxy-Connection: x\nProxy-Authenticate: x\nProxy-Authorization: x\nTE: trailers\nTrailer: x")),
        ok("expect", "PUT /u HTTP/1.1\nHost: a\nContent-Length: 3\nExpect: 100-continue\n\n"),
        ok("request-ids", &get("X-Request-Id: 1\nx-request-id: 2\nX-REQUEST-ID: 3")),
        ok("duplicates-apart", &get("Cookie: a=1\nAccept: x\nCookie: b=2\nAccept: y")),
        ok("case-duplicates", &get("X-A: 1\nx-a: 2\nX-a: 3")),
        ok("many-duplicates", &get(&"X-Same: 1\n".repeat(60))),
        ok("huge-value", &get(&format!("X: {}", "v".repeat(100_000)))),
        ok("huge-value-with-tab", &get(&format!("X: {}\t{}", "v".repeat(9), "w".repeat(50_000)))),
        ok("long-name-and-value", &get(&format!("{}: {}", "n".repeat(200), "v".repeat(1000)))),
        ok("value-with-colons", &get("Referer: http://a/b:c\nX: :::")),
    ];
    for n in [1usize, 2, 63, 64] {
        c.push(ok("many-headers", &format!("GET / HTTP/1.1\nHost: a\n{}\n", numbered(n - 1))));
    }
    for n in [65usize, 256, 257] {
        c.push(miss("too-many-headers", &format!("GET / HTTP/1.1\nHost: a\n{}\n", numbered(n - 1)), M::TooManyHeaders));
    }
    c
}

pub fn corpus() -> Vec<Case> {
    [shapes(), framing(), line_and_bytes()].concat()
}
