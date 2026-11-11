//! What the lane takes from a scanned request head: the facts of the shapes it serves, and a refusal for the rest.

use oxiroute_h1_head::{
    Class, Connection, Header, RequestHead, RequestLine, Scan, Span, scan_request_fields,
    scan_request_line,
};

use crate::observability::logging::valid_correlation_id;

/// The body a request announces.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Body {
    None,
    /// A body of this many bytes (not zero).
    Length(u64),
    Chunked,
}

/// A request head the lane can serve: HTTP/1.1, origin-form, a body framed by one `Content-Length` or by `chunked`, no
/// upgrade, no `Expect`, and a `Connection` that is absent, `keep-alive` or `close`. Small and `Copy`, so it can be
/// held across an await.
#[derive(Clone, Copy, Debug)]
pub(super) struct Request {
    pub(super) method: Span,
    pub(super) path: Span,
    pub(super) host: Span,
    /// The first `X-Request-Id`, when it is a valid id: the one the request goes on with.
    pub(super) client_id: Option<Span>,
    /// The request carries that one id and no other, so nothing is stamped on it.
    pub(super) id_stamped: bool,
    pub(super) close: bool,
    pub(super) head_len: usize,
    pub(super) is_head: bool,
    pub(super) body: Body,
}

impl Request {
    pub(super) fn method<'a>(&self, buf: &'a [u8]) -> &'a [u8] {
        self.method.of(buf)
    }

    /// Whether the request is all there is in `read` bytes: nothing of another request follows it. A chunked body
    /// ends where its framing says, which only the body reader knows.
    pub(super) fn admits(&self, read: usize) -> bool {
        let rest = read.saturating_sub(self.head_len);
        read >= self.head_len
            && match self.body {
                Body::None => rest == 0,
                Body::Length(length) => u64::try_from(rest).is_ok_and(|rest| rest <= length),
                Body::Chunked => true,
            }
    }

    /// The path of the target, which is ASCII (the scanner refuses the rest).
    pub(super) fn path<'a>(&self, buf: &'a [u8]) -> &'a str {
        std::str::from_utf8(self.path.of(buf)).unwrap_or_default()
    }

    pub(super) fn host<'a>(&self, buf: &'a [u8]) -> Option<&'a str> {
        std::str::from_utf8(self.host.of(buf))
            .ok()
            .filter(|host| host.is_ascii())
    }
}

/// The request line of a head: what routes a request, read before the header fields.
#[derive(Clone, Copy, Debug)]
pub(super) struct Line {
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
pub(super) fn line(buf: &[u8]) -> Option<Line> {
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
pub(super) fn scan(buf: &[u8], line: Line, head: &mut RequestHead) -> Option<Request> {
    if scan_request_fields(buf, line.line, head) != Scan::Head {
        return None;
    }
    let facts = head.facts;
    // The scanner reads no length when there is a `Transfer-Encoding`; a request with both is the classic path's.
    let body = match (facts.transfer_encoding, facts.content_length) {
        (true, _) if head.find(Class::ContentLength).is_some() => return None,
        (true, _) => Body::Chunked,
        (false, None | Some(0)) => Body::None,
        (false, Some(length)) => Body::Length(length),
    };
    let plain = !(facts.upgrade || facts.expect) && facts.hop_by_hop == 0;
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
        body,
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

    fn body_of(request: &str) -> Option<Body> {
        let buffer = request.as_bytes();
        let line = line(buffer)?;
        let mut head = RequestHead::new();
        scan(buffer, line, &mut head).map(|request| request.body)
    }

    const HEAD: &str = "POST /r HTTP/1.1\r\nHost: a.test\r\n";

    #[test]
    fn a_request_names_the_framing_of_its_body() {
        assert_eq!(body_of(&format!("{HEAD}\r\n")), Some(Body::None));
        assert_eq!(
            body_of(&format!("{HEAD}Content-Length: 0\r\n\r\n")),
            Some(Body::None)
        );
        assert_eq!(
            body_of(&format!("{HEAD}Content-Length: 5\r\n\r\nhello")),
            Some(Body::Length(5))
        );
        assert_eq!(
            body_of(&format!("{HEAD}Transfer-Encoding: chunked\r\n\r\n")),
            Some(Body::Chunked)
        );
    }

    #[test]
    fn ambiguous_or_unserved_framing_is_left_to_the_classic_path() {
        let both = format!("{HEAD}Content-Length: 5\r\nTransfer-Encoding: chunked\r\n\r\n");
        assert_eq!(body_of(&both), None);
        let expect = format!("{HEAD}Content-Length: 5\r\nExpect: 100-continue\r\n\r\n");
        assert_eq!(body_of(&expect), None);
        let upgrade =
            format!("{HEAD}Content-Length: 5\r\nUpgrade: x\r\nConnection: Upgrade\r\n\r\n");
        assert_eq!(body_of(&upgrade), None);
    }

    #[test]
    fn a_request_is_admitted_when_no_other_request_follows_its_body() {
        let request = |body| Request {
            method: Span { start: 0, len: 4 },
            path: Span { start: 5, len: 2 },
            host: Span { start: 0, len: 0 },
            client_id: None,
            id_stamped: false,
            close: false,
            head_len: 10,
            is_head: false,
            body,
        };
        assert!(request(Body::None).admits(10));
        assert!(!request(Body::None).admits(11));
        assert!(request(Body::Length(5)).admits(15));
        assert!(!request(Body::Length(5)).admits(16));
        assert!(request(Body::Length(5)).admits(10));
        assert!(request(Body::Chunked).admits(10_000));
    }
}
