//! What the lane takes from a scanned request head: the facts of the shapes it serves, and a refusal for the rest.

use std::cell::RefCell;

use http::uri::Authority;
use oxiroute_h1_head::{
    Class, Connection, Header, RequestHead, RequestLine, Scan, Span, scan_request_fields,
    scan_request_line,
};

use crate::observability::logging::valid_correlation_id;

/// A request head the lane can serve: HTTP/1.1, origin-form, a body framed by one `Content-Length`, no upgrade, no `Expect`, and a `Connection` that is absent,
/// `keep-alive` or `close`. Small and `Copy`, so it can be held across an await.
#[derive(Clone, Copy, Debug)]
pub(in crate::http::proxy) struct Request {
    pub(in crate::http::proxy) method: Span,
    pub(in crate::http::proxy) path: Span,
    pub(in crate::http::proxy) host: Span,
    /// The first `X-Request-Id`, when it is a valid id: the one the request goes on with.
    pub(in crate::http::proxy) client_id: Option<Span>,
    /// The request carries that one id and no other, so nothing is stamped on it.
    pub(in crate::http::proxy) id_stamped: bool,
    pub(in crate::http::proxy) close: bool,
    pub(in crate::http::proxy) head_len: usize,
    pub(in crate::http::proxy) is_head: bool,
    /// The bytes of the body that follow the head.
    pub(in crate::http::proxy) body_len: usize,
}

impl Request {
    pub(in crate::http::proxy) fn method<'a>(&self, buf: &'a [u8]) -> &'a [u8] {
        self.method.of(buf)
    }

    /// Whether the lane may serve the request given the `read` bytes that arrived: nothing of another request follows
    /// its body. A body that has not all arrived is read from the client as it comes (see `upload`).
    pub(super) const fn admits(&self, read: usize) -> bool {
        read <= self.head_len + self.body_len
    }

    /// The body, in the bytes `admits` was asked about.
    pub(super) fn body<'a>(&self, buf: &'a [u8]) -> &'a [u8] {
        &buf[self.head_len..]
    }

    /// The path of the target, which is ASCII (the scanner refuses the rest).
    pub(in crate::http::proxy) fn path<'a>(&self, buf: &'a [u8]) -> &'a str {
        std::str::from_utf8(self.path.of(buf)).unwrap_or_default()
    }

    pub(in crate::http::proxy) fn host<'a>(&self, buf: &'a [u8]) -> Option<&'a str> {
        std::str::from_utf8(self.host.of(buf))
            .ok()
            .filter(|host| host.is_ascii())
    }
}

/// Whether the request carries a header that makes the response of a file depend on more than the file: a range or one
/// of the conditions of RFC 9110. The classic path decides those.
pub(in crate::http::proxy) fn is_conditional(head: &RequestHead, buf: &[u8]) -> bool {
    head.headers()
        .iter()
        .any(|header| header.class == Class::Other && is_condition(header.name(buf)))
}

fn is_condition(name: &[u8]) -> bool {
    let names: &[&[u8]] = match name.len() {
        5 => &[b"range"],
        8 => &[b"if-match", b"if-range"],
        13 => &[b"if-none-match"],
        17 => &[b"if-modified-since"],
        19 => &[b"if-unmodified-since"],
        _ => &[],
    };
    names.iter().any(|known| name.eq_ignore_ascii_case(known))
}

/// Authorities a thread has parsed lately: the hosts a listener serves are few, and every request names one.
const KNOWN_AUTHORITIES: usize = 4;

thread_local! {
    static AUTHORITIES: RefCell<(Vec<Authority>, usize)> = const { RefCell::new((Vec::new(), 0)) };
}

/// The authority `host` parses to, or `None` when it is not one: the parse is `Authority::from_str`, remembered for the
/// next request that names the same host.
pub(in crate::http::proxy) fn authority(host: &str) -> Option<Authority> {
    AUTHORITIES.with(|known| {
        let mut known = known.borrow_mut();
        if let Some(found) = known.0.iter().find(|known| known.as_str() == host) {
            return Some(found.clone());
        }
        let parsed = host.parse::<Authority>().ok()?;
        let (list, next) = &mut *known;
        if list.len() < KNOWN_AUTHORITIES {
            list.push(parsed.clone());
        } else {
            list[*next] = parsed.clone();
            *next = (*next + 1) % KNOWN_AUTHORITIES;
        }
        Some(parsed)
    })
}

/// The request line of a head: what routes a request, read before the header fields.
#[derive(Clone, Copy, Debug)]
pub(in crate::http::proxy) struct Line {
    line: RequestLine,
    path: Span,
}

impl Line {
    pub(super) fn fields_start(&self) -> usize {
        self.line.fields_start()
    }

    pub(super) fn method<'a>(&self, buf: &'a [u8]) -> &'a [u8] {
        self.line.method.of(buf)
    }

    /// The path of the target, which is ASCII (the scanner refuses the rest).
    pub(super) fn path<'a>(&self, buf: &'a [u8]) -> &'a str {
        std::str::from_utf8(self.path.of(buf)).unwrap_or_default()
    }
}

/// `None` when the request line is partial or refused: the classic path reads the request.
pub(in crate::http::proxy) fn line(buf: &[u8]) -> Option<Line> {
    let line = scan_request_line(buf).ok()?;
    let target = line.target;
    let query = target.of(buf).iter().position(|byte| *byte == b'?');
    let path = Span {
        start: target.start,
        len: query.map_or(target.len, |at| u32::try_from(at).unwrap_or(target.len)),
    };
    Some(Line { line, path })
}

/// `None` when the classic path must see the request: it is partial, refused or not of the lane's shape. `head`
/// describes the headers of the request when it is the lane's.
pub(in crate::http::proxy) fn scan(
    buf: &[u8],
    line: Line,
    head: &mut RequestHead,
) -> Option<Request> {
    scan_with(buf, line, head, false)
}

/// [`scan`] for the head of an HTTP/2 request written as HTTP/1 text. `Expect` is a field like any other there: the
/// classic HTTP/2 session never answers it, and the origin gets it as it was sent.
pub(in crate::http::proxy) fn scan_h2(
    buf: &[u8],
    line: Line,
    head: &mut RequestHead,
) -> Option<Request> {
    scan_with(buf, line, head, true)
}

fn scan_with(
    buf: &[u8],
    line: Line,
    head: &mut RequestHead,
    expect_is_plain: bool,
) -> Option<Request> {
    if scan_request_fields(buf, line.line, head) != Scan::Head {
        return None;
    }
    let facts = head.facts;
    // The scanner reads no length when there is a `Transfer-Encoding`: a chunked body is the classic path's.
    if facts.transfer_encoding {
        return None;
    }
    let body_len = usize::try_from(facts.content_length.unwrap_or(0)).ok()?;
    let plain = !(facts.upgrade || facts.expect && !expect_is_plain) && facts.hop_by_hop == 0;
    let close = match facts.connection {
        Connection::Absent | Connection::KeepAlive => false,
        Connection::Close => true,
        Connection::Other => return None,
    };
    if !plain {
        return None;
    }
    let client_id = (facts.request_id_count >= 1)
        .then(|| head.headers()[usize::from(facts.request_id)])
        .filter(|header| header.class == Class::XRequestId)
        .map(|header| value_span(&header))
        .filter(|span| std::str::from_utf8(span.of(buf)).is_ok_and(valid_correlation_id));
    Some(Request {
        method: head.method,
        path: line.path,
        host: value_span(&head.headers()[usize::from(facts.host)]),
        client_id,
        id_stamped: client_id.is_some() && facts.request_id_count == 1,
        close,
        head_len: head.head_len as usize,
        is_head: head.method(buf) == b"HEAD",
        body_len,
    })
}

fn value_span(header: &Header) -> Span {
    Span {
        start: header.value_start,
        len: header.value_len,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body_len_of(request: &str) -> Option<usize> {
        let buffer = request.as_bytes();
        let line = line(buffer)?;
        let mut head = RequestHead::new();
        scan(buffer, line, &mut head).map(|request| request.body_len)
    }

    const HEAD: &str = "POST /r HTTP/1.1\r\nHost: a.test\r\n";

    #[test]
    fn a_remembered_authority_is_the_one_the_parser_gives() {
        let hosts = [
            "a.test",
            "b.test:8080",
            "[::1]:80",
            "c.test",
            "d.test",
            "e.test:1",
            "a.test",
            "b.test:8080",
        ];
        for round in 0..3 {
            for host in hosts {
                let remembered = authority(host).expect("an authority");
                assert_eq!(
                    remembered,
                    host.parse::<Authority>().unwrap(),
                    "{round} {host}"
                );
            }
        }
        for refused in ["", "a b", "a.test/x", "[::1", "a@b@c"] {
            assert_eq!(
                authority(refused),
                refused.parse::<Authority>().ok(),
                "{refused:?}"
            );
        }
    }

    fn conditional(request: &str) -> bool {
        let buffer = request.as_bytes();
        let mut head = RequestHead::new();
        scan(buffer, line(buffer).expect("line"), &mut head).expect("a head the lane scans");
        is_conditional(&head, buffer)
    }

    #[test]
    fn a_range_or_a_condition_makes_a_request_conditional_whatever_its_case() {
        for name in [
            "Range",
            "RANGE",
            "If-Match",
            "if-range",
            "If-None-Match",
            "if-modified-since",
            "IF-UNMODIFIED-SINCE",
        ] {
            assert!(conditional(&format!("{HEAD}{name}: x\r\n\r\n")), "{name}");
        }
        for name in ["Ranges", "If", "If-Matches", "X-Range", "Accept", "If-None"] {
            assert!(!conditional(&format!("{HEAD}{name}: x\r\n\r\n")), "{name}");
        }
        assert!(!conditional(&format!("{HEAD}\r\n")));
    }

    #[test]
    fn a_request_names_the_length_of_its_body() {
        assert_eq!(body_len_of(&format!("{HEAD}\r\n")), Some(0));
        assert_eq!(
            body_len_of(&format!("{HEAD}Content-Length: 0\r\n\r\n")),
            Some(0)
        );
        assert_eq!(
            body_len_of(&format!("{HEAD}Content-Length: 5\r\n\r\nhello")),
            Some(5)
        );
    }

    #[test]
    fn chunked_or_ambiguous_or_unserved_framing_is_left_to_the_classic_path() {
        let chunked = format!("{HEAD}Transfer-Encoding: chunked\r\n\r\n");
        assert_eq!(body_len_of(&chunked), None);
        let both = format!("{HEAD}Content-Length: 5\r\nTransfer-Encoding: chunked\r\n\r\n");
        assert_eq!(body_len_of(&both), None);
        let expect = format!("{HEAD}Content-Length: 5\r\nExpect: 100-continue\r\n\r\n");
        assert_eq!(body_len_of(&expect), None);
        let upgrade =
            format!("{HEAD}Content-Length: 5\r\nUpgrade: x\r\nConnection: Upgrade\r\n\r\n");
        assert_eq!(body_len_of(&upgrade), None);
    }

    #[test]
    fn a_request_is_admitted_when_no_other_request_follows_its_body() {
        let request = |body_len| Request {
            method: Span { start: 0, len: 4 },
            path: Span { start: 5, len: 2 },
            host: Span { start: 0, len: 0 },
            client_id: None,
            id_stamped: false,
            close: false,
            head_len: 10,
            is_head: false,
            body_len,
        };
        assert!(request(0).admits(10));
        assert!(!request(0).admits(11));
        assert!(request(5).admits(15));
        assert!(!request(5).admits(16));
        assert!(
            request(5).admits(12),
            "a body that is still coming is read as it arrives"
        );
    }
}
