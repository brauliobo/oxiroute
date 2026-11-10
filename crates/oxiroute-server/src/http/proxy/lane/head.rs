//! What the lane takes from a scanned request head: the facts of the shapes it serves, and a refusal for the rest.

use oxiroute_h1_head::{Class, Connection, Header, RequestHead, Scan, Span, scan_request};

use crate::observability::logging::valid_correlation_id;

/// A request head the lane can serve: HTTP/1.1, origin-form, no body, no upgrade, no `Expect`, and a `Connection`
/// that is absent, `keep-alive` or `close`. Small and `Copy`, so it can be held across an await.
#[derive(Clone, Copy, Debug)]
pub(super) struct Request {
    pub(super) method: Span,
    pub(super) path: Span,
    pub(super) host: Span,
    pub(super) request_id: Option<Span>,
    pub(super) close: bool,
    pub(super) head_len: usize,
    pub(super) is_head: bool,
}

impl Request {
    pub(super) fn method<'a>(&self, buf: &'a [u8]) -> &'a [u8] {
        self.method.of(buf)
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

/// `None` when the classic path must see the request: it is partial, refused or not of the lane's shape.
pub(super) fn scan(buf: &[u8]) -> Option<Request> {
    let mut head = RequestHead::new();
    if scan_request(buf, &mut head) != Scan::Head {
        return None;
    }
    let facts = head.facts;
    let bodyless = facts.content_length.is_none_or(|length| length == 0);
    let plain = !(facts.transfer_encoding || facts.upgrade || facts.expect);
    let close = match facts.connection {
        Connection::Absent | Connection::KeepAlive => false,
        Connection::Close => true,
        Connection::Other => return None,
    };
    if !(bodyless && plain) {
        return None;
    }
    let target = head.target;
    let query = head.target(buf).iter().position(|byte| *byte == b'?');
    let path = Span {
        start: target.start,
        len: query.map_or(target.len, |at| u32::try_from(at).unwrap_or(target.len)),
    };
    let request_id = (facts.request_id_count == 1)
        .then(|| head.headers()[usize::from(facts.request_id)])
        .filter(|header| header.class == Class::XRequestId)
        .map(|header| value_span(&header))
        .filter(|span| std::str::from_utf8(span.of(buf)).is_ok_and(valid_correlation_id));
    Some(Request {
        method: head.method,
        path,
        host: value_span(&head.headers()[usize::from(facts.host)]),
        request_id,
        close,
        head_len: head.head_len as usize,
        is_head: head.method(buf) == b"HEAD",
    })
}

fn value_span(header: &Header) -> Span {
    Span {
        start: header.value_start,
        len: header.value_len,
    }
}
