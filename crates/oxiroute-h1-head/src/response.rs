//! The response scanner: the common clean shape of an upstream response head, or "ask the classic parser".

use crate::class::{Class, HOP_BY_HOP_RESPONSE};
use crate::fields::{Block, Header, MAX_HEADERS, Stop, scan_fields};
use crate::miss::{MissReason, ScanResponse};
use crate::request::{Connection, Span, chunked_final, connection, content_length};

/// What a response head says about framing and connection handling.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResponseFacts {
    /// The one `Content-Length`, absent when there is none or a `Transfer-Encoding` makes classic drop it.
    pub content_length: Option<u64>,
    pub transfer_encoding: bool,
    /// The last coding of the last `Transfer-Encoding` is `chunked`.
    pub chunked_final: bool,
    /// `Absent`, `KeepAlive` or `Close`; anything else is [`ScanResponse::Slow`].
    pub connection: Connection,
    pub upgrade: bool,
    /// [`HOP_KEEP_ALIVE`](crate::HOP_KEEP_ALIVE) and the other `HOP_*` bits up to [`HOP_UPGRADE`](crate::HOP_UPGRADE).
    pub hop_by_hop: u16,
    pub request_id: bool,
    pub upstream_request_id: bool,
    /// Status 1xx.
    pub informational: bool,
    pub has_underscore: bool,
    pub has_uppercase: bool,
    /// Two headers share a name, in any case: `HeaderMap` groups them, so the written order is not the arrival order.
    pub duplicate_names: bool,
    /// Bit [`Class::bit`] set for each known name present.
    pub present: u32,
}

/// A scanned response head. Reuse one per upstream connection.
#[derive(Clone, Debug)]
pub struct ResponseHead {
    pub status: u16,
    /// 0 or 1.
    pub minor: u8,
    /// The reason phrase as sent: tabs, spaces and visible ASCII, possibly empty.
    pub reason: Span,
    /// Bytes of the status line, CRLF included.
    pub line_len: u32,
    /// Bytes through the blank line.
    pub head_len: u32,
    pub count: u8,
    pub facts: ResponseFacts,
    headers: [Header; MAX_HEADERS],
}

impl Default for ResponseHead {
    fn default() -> Self {
        Self::new()
    }
}

impl ResponseHead {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            status: 0,
            minor: 1,
            reason: Span { start: 0, len: 0 },
            line_len: 0,
            head_len: 0,
            count: 0,
            facts: ResponseFacts {
                content_length: None,
                transfer_encoding: false,
                chunked_final: false,
                connection: Connection::Absent,
                upgrade: false,
                hop_by_hop: 0,
                request_id: false,
                upstream_request_id: false,
                informational: false,
                has_underscore: false,
                has_uppercase: false,
                duplicate_names: false,
                present: 0,
            },
            headers: [Header {
                name_start: 0,
                value_start: 0,
                value_len: 0,
                name_len: 0,
                class: Class::Other,
                flags: 0,
            }; MAX_HEADERS],
        }
    }

    /// The headers in arrival order.
    #[inline]
    #[must_use]
    pub fn headers(&self) -> &[Header] {
        &self.headers[..usize::from(self.count)]
    }

    /// The first header of `class`.
    #[inline]
    #[must_use]
    pub fn find(&self, class: Class) -> Option<&Header> {
        self.headers().iter().find(|h| h.class == class)
    }

    #[inline]
    #[must_use]
    pub fn reason<'a>(&self, buf: &'a [u8]) -> &'a [u8] {
        self.reason.of(buf)
    }
}

#[inline(always)]
fn bad<T>(reason: MissReason) -> Result<T, Stop> {
    Err(Stop::Bad(reason))
}

const STATUS_LINE_START: &[u8; 9] = b"HTTP/1.1 ";

/// The status, the minor version and the reason span of the status line, and the offset after it.
fn status_line(buf: &[u8]) -> Result<(u16, u8, Span, usize), Stop> {
    for (i, &c) in buf.iter().take(9).enumerate() {
        let expected = if i == 7 {
            c == b'0' || c == b'1'
        } else {
            c == STATUS_LINE_START[i]
        };
        if !expected {
            return bad(MissReason::Version);
        }
    }
    let mut status = 0u16;
    for i in 9..12 {
        match buf.get(i) {
            Some(c @ b'0'..=b'9') => status = status * 10 + u16::from(c - b'0'),
            Some(_) => return bad(MissReason::Version),
            None => return Err(Stop::Partial),
        }
    }
    match buf.get(12) {
        Some(b' ') => {}
        Some(_) => return bad(MissReason::Version),
        None => return Err(Stop::Partial),
    }
    if !(100..=599).contains(&status) {
        return bad(MissReason::Version);
    }
    let mut end = 13;
    while matches!(buf.get(end), Some(b'\t' | b' '..=b'~')) {
        end += 1;
    }
    match (buf.get(end), buf.get(end + 1)) {
        (Some(b'\r'), Some(b'\n')) => {}
        (Some(b'\r'), None) | (None, _) => return Err(Stop::Partial),
        _ => return bad(MissReason::Control),
    }
    Ok((
        status,
        buf[7] - b'0',
        Span {
            start: 13,
            len: (end - 13) as u32,
        },
        end + 2,
    ))
}

fn scan(buf: &[u8], out: &mut ResponseHead) -> Result<(), Stop> {
    let (status, minor, reason, fields_start) = status_line(buf)?;
    let mut block = Block::new();
    scan_fields(buf, fields_start, &mut out.headers, &mut block)?;
    let facts = &mut out.facts;
    let has = |class: Class| block.seen & class.bit() != 0;
    let duplicated = |class: Class| block.dup & class.bit() != 0;
    facts.content_length = None;
    if has(Class::ContentLength) {
        let h = &out.headers[usize::from(block.first[Class::ContentLength as usize])];
        let length = content_length(h.value(buf)).filter(|_| !duplicated(Class::ContentLength));
        facts.content_length = Some(length.ok_or(Stop::Bad(MissReason::ContentLength))?);
    }
    facts.connection = Connection::Absent;
    if has(Class::Connection) {
        let h = &out.headers[usize::from(block.first[Class::Connection as usize])];
        facts.connection = connection(buf, h, duplicated(Class::Connection));
        if facts.connection == Connection::Other {
            return bad(MissReason::Name);
        }
    }
    facts.transfer_encoding = has(Class::TransferEncoding);
    facts.chunked_final = false;
    if facts.transfer_encoding {
        let last = out.headers[..usize::from(block.count)]
            .iter()
            .rfind(|h| h.class == Class::TransferEncoding);
        facts.chunked_final = last.is_some_and(|h| chunked_final(h.value(buf)));
        facts.content_length = None;
    }
    facts.upgrade = has(Class::Upgrade);
    facts.hop_by_hop = ((block.seen >> Class::KeepAlive as u32) as u16) & HOP_BY_HOP_RESPONSE;
    facts.request_id = has(Class::XRequestId);
    facts.upstream_request_id = has(Class::XUpstreamRequestId);
    facts.informational = status < 200;
    facts.has_underscore = block.underscore();
    facts.has_uppercase = block.upper();
    facts.duplicate_names = block.duplicate_names;
    facts.present = block.seen;
    out.status = status;
    out.minor = minor;
    out.reason = reason;
    out.line_len = fields_start as u32;
    out.head_len = block.head_len;
    out.count = block.count;
    Ok(())
}

/// Scans the response head at the start of `buf`.
///
/// `Head` is only the common clean shape; `Slow` means "not that, or unsure": the classic parser decides, and it may
/// well accept the bytes. See the crate documentation for the shape.
#[must_use]
pub fn scan_response(buf: &[u8], out: &mut ResponseHead) -> ScanResponse {
    if u32::try_from(buf.len()).is_err() {
        return ScanResponse::Slow;
    }
    match scan(buf, out) {
        Ok(()) => ScanResponse::Head,
        Err(Stop::Partial) => ScanResponse::Partial,
        Err(Stop::Bad(_)) => ScanResponse::Slow,
    }
}
