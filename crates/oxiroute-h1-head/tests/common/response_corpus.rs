//! Hand-written response heads with the verdict the scanner must give. `\n` in a text is sent as CRLF.

use super::request_corpus::crlf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Head,
    Slow,
}

pub type Case = (&'static str, Vec<u8>, Verdict);

fn head(name: &'static str, text: &str) -> Case {
    (name, crlf(text), Verdict::Head)
}

fn slow(name: &'static str, text: &str) -> Case {
    (name, crlf(text), Verdict::Slow)
}

fn raw(name: &'static str, bytes: &[u8], verdict: Verdict) -> Case {
    (name, bytes.to_vec(), verdict)
}

fn ok(headers: &str) -> String {
    format!("HTTP/1.1 200 OK\n{headers}\n\n")
}

fn numbered(n: usize) -> String {
    (0..n).fold(String::new(), |all, i| all + &format!("X-H{i}: v{i}\n"))
}

#[rustfmt::skip]
pub fn corpus() -> Vec<Case> {
    let mut c = vec![
        head("minimal", "HTTP/1.1 200 OK\n\n"),
        head("typical", "HTTP/1.1 200 OK\ncontent-type: application/octet-stream\ncontent-length: 1024\ndate: Mon, 09 Nov 2026 20:00:00 GMT\n\n"),
        head("browser-like", "HTTP/1.1 200 OK\nServer: origin\nDate: Mon, 09 Nov 2026 20:00:00 GMT\nContent-Type: application/javascript\nContent-Length: 1024\nConnection: keep-alive\nCache-Control: public, max-age=3600\nETag: \"0123456789abcdef\"\nLast-Modified: Mon, 09 Nov 2026 10:00:00 GMT\nVary: Accept-Encoding\nX-Content-Type-Options: nosniff\n\n"),
        head("http10", "HTTP/1.0 200 OK\nContent-Length: 3\n\n"),
        head("not-found", "HTTP/1.1 404 Not Found\nContent-Length: 0\n\n"),
        head("no-content", "HTTP/1.1 204 No Content\n\n"),
        head("not-modified", "HTTP/1.1 304 Not Modified\nETag: x\n\n"),
        head("continue", "HTTP/1.1 100 Continue\n\n"),
        head("early-hints", "HTTP/1.1 103 Early Hints\nLink: </a.css>; rel=preload\n\n"),
        head("unknown-status", "HTTP/1.1 299 Whatever\n\n"),
        head("custom-reason", "HTTP/1.1 200 Totally Fine \t!\n\n"),
        head("empty-reason", "HTTP/1.1 200 \n\n"),
        head("status-599", "HTTP/1.1 599 Edge\n\n"),
        head("mismatched-reason", "HTTP/1.1 404 OK\n\n"),
        head("switching-protocols", "HTTP/1.1 101 Switching Protocols\nUpgrade: websocket\n\n"),
        slow("switching-protocols-connection", "HTTP/1.1 101 Switching Protocols\nUpgrade: websocket\nConnection: Upgrade\n\n"),
        head("chunked", &ok("Transfer-Encoding: chunked")),
        head("chunked-upper", &ok("TRANSFER-ENCODING: Chunked")),
        head("gzip-chunked", &ok("Transfer-Encoding: gzip, chunked")),
        head("chunked-gzip", &ok("Transfer-Encoding: chunked, gzip")),
        head("te-and-length", &ok("Content-Length: 5\nTransfer-Encoding: chunked")),
        slow("te-and-dup-length", &ok("Content-Length: 5\nContent-Length: 6\nTransfer-Encoding: chunked")),
        head("te-last-header-wins", &ok("Transfer-Encoding: chunked\nTransfer-Encoding: gzip")),
        head("length-zero", &ok("Content-Length: 0")),
        head("length-padded", &ok("Content-Length:  12 \t")),
        slow("length-duplicate", &ok("Content-Length: 5\nContent-Length: 5")),
        slow("length-invalid", &ok("Content-Length: abc")),
        slow("length-signed", &ok("Content-Length: +5")),
        slow("length-comma", &ok("Content-Length: 5, 5")),
        slow("length-empty", &ok("Content-Length:")),
        slow("length-19-digits", &ok("Content-Length: 9999999999999999999")),
        head("connection-close", &ok("Connection: close")),
        head("connection-keep-alive", &ok("CONNECTION: Keep-Alive\nKeep-Alive: timeout=5, max=100")),
        slow("connection-other", &ok("Connection: x-hop")),
        slow("connection-list", &ok("Connection: close, x-hop")),
        slow("connection-twice", &ok("Connection: close\nConnection: close")),
        head("hop-by-hop", &ok("Keep-Alive: 5\nProxy-Connection: x\nProxy-Authenticate: x\nProxy-Authorization: x\nTE: x\nTrailer: x\nUpgrade: h2c")),
        head("request-ids", &ok("X-Request-Id: up-1\nX-Upstream-Request-Id: up-0")),
        head("set-cookies", &ok("Set-Cookie: a=1\nSet-Cookie: b=2\nCache-Control: no-store\nset-cookie: c=3")),
        head("duplicates-apart", &ok("Vary: a\nX-B: 1\nVary: b\nX-B: 2")),
        head("date-and-connection-twice", &ok("Date: a\nConnection: close\nDate: b")),
        head("empty-values", &ok("X:\nY: \nZ:  ")),
        head("tab-in-value", "HTTP/1.1 200 OK\nX: a\tb\t\n\n"),
        head("high-bytes-value", "HTTP/1.1 200 OK\nX: caf\u{e9}\n\n"),
        head("huge-value", &ok(&format!("X: {}", "v".repeat(100_000)))),
        head("underscore-name", &ok("X_Under: 1")),
        slow("no-reason", "HTTP/1.1 200\n\n"),
        slow("no-space", "HTTP/1.1200 OK\n\n"),
        slow("two-spaces", "HTTP/1.1  200 OK\n\n"),
        slow("status-99", "HTTP/1.1 099 Low\n\n"),
        slow("status-600", "HTTP/1.1 600 High\n\n"),
        slow("status-2-digits", "HTTP/1.1 20 OK\n\n"),
        slow("status-4-digits", "HTTP/1.1 2000 OK\n\n"),
        slow("status-letters", "HTTP/1.1 abc OK\n\n"),
        slow("http2", "HTTP/2 200 OK\n\n"),
        slow("http12", "HTTP/1.2 200 OK\n\n"),
        slow("http09", "HTTP/0.9 200 OK\n\n"),
        slow("lowercase-version", "http/1.1 200 OK\n\n"),
        slow("leading-crlf", "\nHTTP/1.1 200 OK\n\n"),
        raw("bare-lf-status", b"HTTP/1.1 200 OK\nX: y\r\n\r\n", Verdict::Slow),
        raw("bare-lf-header", b"HTTP/1.1 200 OK\r\nX: y\nZ: w\r\n\r\n", Verdict::Slow),
        raw("bare-lf-all", b"HTTP/1.1 200 OK\nX: y\n\n", Verdict::Slow),
        raw("bare-cr-value", b"HTTP/1.1 200 OK\r\nX: y\rz\r\n\r\n", Verdict::Slow),
        raw("bare-cr-reason", b"HTTP/1.1 200 O\rK\r\n\r\n", Verdict::Slow),
        raw("high-byte-reason", b"HTTP/1.1 200 caf\xc3\xa9\r\n\r\n", Verdict::Slow),
        raw("control-in-reason", b"HTTP/1.1 200 O\x01K\r\n\r\n", Verdict::Slow),
        raw("nul-value", b"HTTP/1.1 200 OK\r\nX: y\x00z\r\n\r\n", Verdict::Slow),
        raw("del-value", b"HTTP/1.1 200 OK\r\nX: y\x7fz\r\n\r\n", Verdict::Slow),
        raw("high-byte-name", b"HTTP/1.1 200 OK\r\nX\xc3: y\r\n\r\n", Verdict::Slow),
        slow("folding", &ok("X: a\n  b")),
        slow("folding-tab", &ok("X: a\n\tb")),
        slow("folding-first", "HTTP/1.1 200 OK\n X: a\n\n"),
        slow("space-before-colon", &ok("X : a")),
        slow("tab-before-colon", &ok("X\t: a")),
        slow("empty-name", &ok(": a")),
        slow("name-no-colon", &ok("X")),
        slow("name-256", &ok(&format!("{}: y", "n".repeat(256)))),
    ];
    for n in [0usize, 1, 2, 63, 64] {
        c.push(head("many-headers", &ok(&numbered(n))));
    }
    for n in [65usize, 66, 256, 257] {
        c.push(slow("too-many-headers", &ok(&numbered(n))));
    }
    c
}
