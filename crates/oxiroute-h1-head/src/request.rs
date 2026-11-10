//! The request scanner: one pass over a request head, refusing everything the classic parser might read differently.

use crate::class::Class;
use crate::fields::{Header, MAX_HEADERS, Stop, scan_fields};
use crate::miss::{MissReason, Scan};
use crate::tables::{NAME, TARGET};

/// The longest method token the scanner takes.
pub const MAX_METHOD_LEN: usize = 32;
/// The longest target `http::Uri` stores.
pub const MAX_TARGET_LEN: usize = 65_534;
const MAX_CONTENT_LENGTH_DIGITS: usize = 18;

/// Bit of [`RequestFacts::hop_by_hop`]: `Keep-Alive` is present.
pub const HOP_KEEP_ALIVE: u8 = 1 << 0;
/// Bit of [`RequestFacts::hop_by_hop`]: `Proxy-Connection` is present.
pub const HOP_PROXY_CONNECTION: u8 = 1 << 1;
/// Bit of [`RequestFacts::hop_by_hop`]: `Proxy-Authenticate` is present.
pub const HOP_PROXY_AUTHENTICATE: u8 = 1 << 2;
/// Bit of [`RequestFacts::hop_by_hop`]: `Proxy-Authorization` is present.
pub const HOP_PROXY_AUTHORIZATION: u8 = 1 << 3;
/// Bit of [`RequestFacts::hop_by_hop`]: `TE` is present.
pub const HOP_TE: u8 = 1 << 4;
/// Bit of [`RequestFacts::hop_by_hop`]: `Trailer` is present.
pub const HOP_TRAILER: u8 = 1 << 5;
/// All six fixed hop-by-hop fields of a request.
pub const HOP_BY_HOP_REQUEST: u8 = 0x3f;

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
    /// [`HOP_KEEP_ALIVE`] and the other `HOP_*` bits.
    pub hop_by_hop: u8,
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
fn content_length(value: &[u8]) -> Option<u64> {
    if value.is_empty() || value.len() > MAX_CONTENT_LENGTH_DIGITS {
        return None;
    }
    value.iter().try_fold(0u64, |n, &c| {
        c.is_ascii_digit().then(|| n * 10 + u64::from(c - b'0'))
    })
}

/// Whether the last comma-separated coding of a `Transfer-Encoding` value is `chunked`, as the classic check reads it.
fn chunked_final(value: &[u8]) -> bool {
    let last = value
        .iter()
        .rposition(|&c| c == b',')
        .map_or(value, |comma| &value[comma + 1..]);
    last.trim_ascii().eq_ignore_ascii_case(b"chunked")
}

fn connection(buf: &[u8], h: &Header, dup: bool) -> Connection {
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

fn scan(buf: &[u8], out: &mut RequestHead) -> Result<(), Stop> {
    let method_end = method_end(buf)?;
    let target_start = method_end + 1;
    let target_end = target_end(buf, target_start)?;
    let fields_start = version_end(buf, target_end + 1)?;
    let block = scan_fields(buf, fields_start, &mut out.headers)?;
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
    facts.hop_by_hop = ((block.seen >> Class::KeepAlive as u32) as u8) & HOP_BY_HOP_REQUEST;
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
    facts.has_underscore = block.underscore;
    facts.has_uppercase = block.upper;
    facts.duplicate_names = block.duplicate_names;
    facts.present = block.seen;
    out.method = Span {
        start: 0,
        len: method_end as u32,
    };
    out.target = Span {
        start: target_start as u32,
        len: (target_end - target_start) as u32,
    };
    out.minor = 1;
    out.head_len = block.head_len;
    out.count = block.count;
    Ok(())
}

/// Scans the request head at the start of `buf`.
///
/// `Head` means the classic parser accepts the same bytes and reads the same method, target, version, header list and
/// facts; see the crate documentation for what is refused. `out` is only meaningful after `Head`.
#[must_use]
pub fn scan_request(buf: &[u8], out: &mut RequestHead) -> Scan {
    if u32::try_from(buf.len()).is_err() {
        return Scan::Miss(MissReason::TooLarge);
    }
    match scan(buf, out) {
        Ok(()) => Scan::Head,
        Err(Stop::Partial) => Scan::Partial,
        Err(Stop::Bad(reason)) => Scan::Miss(reason),
    }
}
