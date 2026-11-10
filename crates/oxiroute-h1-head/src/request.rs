//! The request scanner: one pass over a request head, refusing everything the classic parser might read differently.

use crate::class::{Class, HOP_BY_HOP_REQUEST};
use crate::fields::{Block, Header, MAX_HEADERS, Stop, scan_fields};
use crate::miss::{MissReason, Scan};
use crate::tables::{NAME, TARGET};

/// The longest method token the scanner takes.
pub const MAX_METHOD_LEN: usize = 32;
/// The longest target `http::Uri` stores.
pub const MAX_TARGET_LEN: usize = 65_534;
const MAX_CONTENT_LENGTH_DIGITS: usize = 18;

/// A byte range of the scanned buffer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub len: u32,
}

impl Span {
    #[inline]
    #[must_use]
    pub fn of(self, buf: &[u8]) -> &[u8] {
        &buf[self.start as usize..(self.start + self.len) as usize]
    }
}

/// What a `Connection` header says, when there is exactly one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Connection {
    #[default]
    Absent,
    /// Exactly `keep-alive`, in any case.
    KeepAlive,
    /// Exactly `close`, in any case.
    Close,
    /// Any other value, or more than one `Connection` header.
    Other,
}

/// What the head says about framing, connection handling and the fields the lane edits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RequestFacts {
    /// Index of the one `Host` header.
    pub host: u8,
    /// The digits of the one `Content-Length`, if present. When a `Transfer-Encoding` is present too, classic ignores it.
    pub content_length: Option<u64>,
    pub transfer_encoding: bool,
    pub upgrade: bool,
    pub expect: bool,
    pub connection: Connection,
    /// [`HOP_KEEP_ALIVE`](crate::HOP_KEEP_ALIVE) and the other `HOP_*` bits up to [`HOP_TRAILER`](crate::HOP_TRAILER).
    pub hop_by_hop: u16,
    /// How many `X-Request-Id` headers there are, up to 255, and the index of the first.
    pub request_id_count: u8,
    pub request_id: u8,
    pub has_underscore: bool,
    /// Some name has a capital letter, so the lowercase form classic writes differs from the bytes sent.
    pub has_uppercase: bool,
    /// Two headers share a name, in any case: `HeaderMap` groups them, so the written order is not the arrival order.
    pub duplicate_names: bool,
    /// Bit [`Class::bit`] set for each known name present.
    pub present: u32,
}

/// A scanned request head. Reuse one per connection: [`scan_request`] overwrites what it needs and keeps no state.
#[derive(Clone, Debug)]
pub struct RequestHead {
    pub method: Span,
    pub target: Span,
    /// Always 1.
    pub minor: u8,
    /// Bytes of the request line, CRLF included.
    pub line_len: u32,
    /// Bytes through the blank line. Bytes after it are not looked at.
    pub head_len: u32,
    pub count: u8,
    pub facts: RequestFacts,
    headers: [Header; MAX_HEADERS],
}

impl Default for RequestHead {
    fn default() -> Self {
        Self::new()
    }
}

impl RequestHead {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            method: Span { start: 0, len: 0 },
            target: Span { start: 0, len: 0 },
            minor: 1,
            line_len: 0,
            head_len: 0,
            count: 0,
            facts: RequestFacts {
                host: 0,
                content_length: None,
                transfer_encoding: false,
                upgrade: false,
                expect: false,
                connection: Connection::Absent,
                hop_by_hop: 0,
                request_id_count: 0,
                request_id: 0,
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

    #[inline]
    #[must_use]
    pub fn method<'a>(&self, buf: &'a [u8]) -> &'a [u8] {
        self.method.of(buf)
    }

    #[inline]
    #[must_use]
    pub fn target<'a>(&self, buf: &'a [u8]) -> &'a [u8] {
        self.target.of(buf)
    }

    /// The first header of `class`.
    #[inline]
    #[must_use]
    pub fn find(&self, class: Class) -> Option<&Header> {
        self.headers().iter().find(|h| h.class == class)
    }

    /// The value of the one `Host` header.
    #[inline]
    #[must_use]
    pub fn host<'a>(&self, buf: &'a [u8]) -> &'a [u8] {
        self.headers[usize::from(self.facts.host)].value(buf)
    }
}

#[inline(always)]
fn miss<T>(reason: MissReason) -> Result<T, Stop> {
    Err(Stop::Bad(reason))
}

fn method_end(buf: &[u8]) -> Result<usize, Stop> {
    let mut i = 0;
    while let Some(&c) = buf.get(i) {
        if NAME[usize::from(c)] == 0 {
            break;
        }
        i += 1;
    }
    match buf.get(i) {
        Some(b' ') if i > 0 && i <= MAX_METHOD_LEN => Ok(i),
        _ if i > MAX_METHOD_LEN => miss(MissReason::MethodTooLong),
        Some(_) => miss(MissReason::Method),
        None => Err(Stop::Partial),
    }
}

fn target_end(buf: &[u8], start: usize) -> Result<usize, Stop> {
    match buf.get(start) {
        Some(b'/') => {}
        Some(_) => return miss(MissReason::Target),
        None => return Err(Stop::Partial),
    }
    let mut i = start + 1;
    while buf.get(i).is_some_and(|&c| TARGET[usize::from(c)]) {
        i += 1;
    }
    if i - start > MAX_TARGET_LEN {
        return miss(MissReason::TargetTooLong);
    }
    match buf.get(i) {
        Some(b' ') => Ok(i),
        Some(_) => miss(MissReason::Target),
        None => Err(Stop::Partial),
    }
}

/// The offset after `HTTP/1.1\r\n` at `start`.
fn version_end(buf: &[u8], start: usize) -> Result<usize, Stop> {
    const VERSION: &[u8; 8] = b"HTTP/1.1";
    let Some(version) = buf.get(start..start + 8) else {
        let rest = &buf[start.min(buf.len())..];
        return if VERSION.starts_with(rest) {
            Err(Stop::Partial)
        } else {
            miss(MissReason::Version)
        };
    };
    if version != VERSION {
        return if version == b"HTTP/1.0" {
            miss(MissReason::Version10)
        } else {
            miss(MissReason::Version)
        };
    }
    match (buf.get(start + 8), buf.get(start + 9)) {
        (Some(b'\r'), Some(b'\n')) => Ok(start + 10),
        (Some(b'\r'), None) | (None, _) => Err(Stop::Partial),
        (Some(b'\n'), _) => miss(MissReason::BareLf),
        (Some(b'\r'), Some(_)) => miss(MissReason::BareCr),
        (Some(_), _) => miss(MissReason::Version),
    }
}

/// The digits of a `Content-Length` value, or `None` for anything that is not 1 to 18 digits.
pub(crate) fn content_length(value: &[u8]) -> Option<u64> {
    if value.is_empty() || value.len() > MAX_CONTENT_LENGTH_DIGITS {
        return None;
    }
    value.iter().try_fold(0u64, |n, &c| {
        c.is_ascii_digit().then(|| n * 10 + u64::from(c - b'0'))
    })
}

/// Whether the last comma-separated coding of a `Transfer-Encoding` value is `chunked`, as the classic check reads it.
pub(crate) fn chunked_final(value: &[u8]) -> bool {
    let last = value
        .iter()
        .rposition(|&c| c == b',')
        .map_or(value, |comma| &value[comma + 1..]);
    last.trim_ascii().eq_ignore_ascii_case(b"chunked")
}

pub(crate) fn connection(buf: &[u8], h: &Header, dup: bool) -> Connection {
    let value = h.value(buf);
    if dup {
        Connection::Other
    } else if value.eq_ignore_ascii_case(b"keep-alive") {
        Connection::KeepAlive
    } else if value.eq_ignore_ascii_case(b"close") {
        Connection::Close
    } else {
        Connection::Other
    }
}

/// The request line at the start of a buffer: where the method and the target are, and where the fields start.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RequestLine {
    pub method: Span,
    pub target: Span,
    fields_start: u32,
}

fn line(buf: &[u8]) -> Result<RequestLine, Stop> {
    let method_end = method_end(buf)?;
    let target_start = method_end + 1;
    let target_end = target_end(buf, target_start)?;
    let fields_start = version_end(buf, target_end + 1)?;
    Ok(RequestLine {
        method: Span {
            start: 0,
            len: method_end as u32,
        },
        target: Span {
            start: target_start as u32,
            len: (target_end - target_start) as u32,
        },
        fields_start: fields_start as u32,
    })
}

fn scan(buf: &[u8], line: RequestLine, out: &mut RequestHead) -> Result<(), Stop> {
    let fields_start = line.fields_start as usize;
    let mut block = Block::new();
    scan_fields(buf, fields_start, &mut out.headers, &mut block)?;
    let facts = &mut out.facts;
    let has = |class: Class| block.seen & class.bit() != 0;
    if !has(Class::Host) {
        return miss(MissReason::HostMissing);
    }
    if block.dup & Class::Host.bit() != 0 {
        return miss(MissReason::HostDuplicate);
    }
    facts.content_length = None;
    if has(Class::ContentLength) {
        let h = &out.headers[usize::from(block.first[Class::ContentLength as usize])];
        let length =
            content_length(h.value(buf)).filter(|_| block.dup & Class::ContentLength.bit() == 0);
        facts.content_length = Some(length.ok_or(Stop::Bad(MissReason::ContentLength))?);
    }
    facts.connection = Connection::Absent;
    if has(Class::Connection) {
        let h = &out.headers[usize::from(block.first[Class::Connection as usize])];
        facts.connection = connection(buf, h, block.dup & Class::Connection.bit() != 0);
    }
    facts.host = block.first[Class::Host as usize];
    facts.transfer_encoding = has(Class::TransferEncoding);
    if facts.transfer_encoding {
        let last = out.headers[..usize::from(block.count)]
            .iter()
            .rfind(|h| h.class == Class::TransferEncoding);
        if !last.is_some_and(|h| chunked_final(h.value(buf))) {
            return miss(MissReason::TransferEncoding);
        }
        facts.content_length = None;
    }
    facts.upgrade = has(Class::Upgrade);
    facts.expect = has(Class::Expect);
    facts.hop_by_hop = ((block.seen >> Class::KeepAlive as u32) as u16) & HOP_BY_HOP_REQUEST;
    facts.request_id_count = 0;
    facts.request_id = block.first[Class::XRequestId as usize];
    if has(Class::XRequestId) {
        facts.request_id_count = if block.dup & Class::XRequestId.bit() != 0 {
            out.headers[..usize::from(block.count)]
                .iter()
                .filter(|h| h.class == Class::XRequestId)
                .count() as u8
        } else {
            1
        };
    }
    facts.has_underscore = block.underscore();
    facts.has_uppercase = block.upper();
    facts.duplicate_names = block.duplicate_names;
    facts.present = block.seen;
    out.method = line.method;
    out.target = line.target;
    out.minor = 1;
    out.line_len = line.fields_start;
    out.head_len = block.head_len;
    out.count = block.count;
    Ok(())
}

fn stopped(stop: Stop) -> Scan {
    match stop {
        Stop::Partial => Scan::Partial,
        Stop::Bad(reason) => Scan::Miss(reason),
    }
}

/// Reads the request line at the start of `buf`, for a caller that needs the method and the target before the fields.
/// The error is `Partial` or `Miss`, as [`scan_request`] would answer for the same bytes, never `Head`; the line is
/// the one [`scan_request_fields`] continues from.
///
/// # Errors
///
/// `Partial` when the line is not complete, `Miss` when it is not the lane's.
pub fn scan_request_line(buf: &[u8]) -> Result<RequestLine, Scan> {
    if u32::try_from(buf.len()).is_err() {
        return Err(Scan::Miss(MissReason::TooLarge));
    }
    line(buf).map_err(stopped)
}

/// Scans the header fields that follow `line`, which [`scan_request_line`] read from the start of the same `buf`.
#[must_use]
pub fn scan_request_fields(buf: &[u8], line: RequestLine, out: &mut RequestHead) -> Scan {
    scan(buf, line, out).map_or_else(stopped, |()| Scan::Head)
}

/// Scans the request head at the start of `buf`.
///
/// `Head` means the classic parser accepts the same bytes and reads the same method, target, version, header list and
/// facts; see the crate documentation for what is refused. `out` is only meaningful after `Head`.
#[must_use]
pub fn scan_request(buf: &[u8], out: &mut RequestHead) -> Scan {
    match scan_request_line(buf) {
        Ok(line) => scan_request_fields(buf, line, out),
        Err(miss) => miss,
    }
}
