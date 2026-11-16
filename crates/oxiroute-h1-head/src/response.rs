//! The response scanner: the common clean shape of an upstream response head, or "ask the classic parser".

use oxiroute_cpu::{Job, Lanes, dispatch};

use crate::class::{Class, HOP_BY_HOP_RESPONSE};
use crate::fields::{Block, Header, MAX_RESPONSE_HEADERS, Stop, scan_fields};
use crate::miss::{MissReason, ScanResponse};
use crate::request::{Connection, Span, chunked_final, connection, content_length};
use crate::tolerant;

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
    pub count: u16,
    pub facts: ResponseFacts,
    headers: [Header; MAX_RESPONSE_HEADERS],
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
            }; MAX_RESPONSE_HEADERS],
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

    /// The fields the `Connection` options of the head name, in the order the options come: every option but `close`,
    /// `keep-alive` and `upgrade`. At most [`MAX_NOMINATED`](crate::MAX_NOMINATED) after
    /// [`scan_response_lenient`], none after [`scan_response`].
    pub fn nominated<'b>(&self, buf: &'b [u8]) -> impl Iterator<Item = &'b [u8]> {
        self.headers()
            .iter()
            .filter(|h| h.class == Class::Connection)
            .flat_map(|h| tolerant::options(h.value(buf)))
            .filter(|option| !tolerant::is_reserved(option))
    }
}

#[inline(always)]
fn bad<T>(reason: MissReason) -> Result<T, Stop> {
    Err(Stop::Bad(reason))
}

/// The highest status the strict scan takes.
const STRICT_STATUS_MAX: u16 = 599;
/// The highest status the classic parser takes: any three digits that make a status code.
const LENIENT_STATUS_MAX: u16 = 999;

/// The byte of `HTTP/1.<0|1> <3 digits> ` a status line has at `index`.
fn status_byte_fits(index: usize, byte: u8) -> bool {
    match index {
        0..=6 => byte == b"HTTP/1."[index],
        7 => matches!(byte, b'0' | b'1'),
        9..=11 => byte.is_ascii_digit(),
        _ => byte == b' ',
    }
}

/// Where a status line that is shorter than its fixed part stops: `Partial` unless a byte it has already is wrong.
fn short_status(buf: &[u8]) -> Stop {
    if buf.iter().enumerate().all(|(i, &c)| status_byte_fits(i, c)) {
        Stop::Partial
    } else {
        Stop::Bad(MissReason::Version)
    }
}

/// The status, the minor version and the reason span of the status line, and the offset after it.
#[inline(always)]
fn status_line<const LENIENT: bool>(buf: &[u8]) -> Result<(u16, u8, Span, usize), Stop> {
    let Some(fixed) = buf.first_chunk::<13>() else {
        return Err(short_status(buf));
    };
    let digits = &fixed[9..12];
    let fits = fixed[..7] == *b"HTTP/1."
        && matches!(fixed[7], b'0' | b'1')
        && fixed[8] == b' '
        && digits.iter().all(u8::is_ascii_digit)
        && fixed[12] == b' ';
    if !fits {
        return bad(MissReason::Version);
    }
    let status = digits
        .iter()
        .fold(0, |status, c| status * 10 + u16::from(c - b'0'));
    let highest = if LENIENT {
        LENIENT_STATUS_MAX
    } else {
        STRICT_STATUS_MAX
    };
    if !(100..=highest).contains(&status) {
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
        fixed[7] - b'0',
        Span {
            start: 13,
            len: (end - 13) as u32,
        },
        end + 2,
    ))
}

#[inline(always)]
fn content_length_fact<const LENIENT: bool>(
    buf: &[u8],
    headers: &[Header],
    block: &Block,
) -> Result<u64, Stop> {
    if LENIENT {
        return tolerant::reconciled_length(buf, headers)
            .ok_or(Stop::Bad(MissReason::ContentLength));
    }
    let h = &headers[usize::from(block.first[Class::ContentLength as usize])];
    let single = block.dup & Class::ContentLength.bit() == 0;
    content_length(h.value(buf))
        .filter(|_| single)
        .ok_or(Stop::Bad(MissReason::ContentLength))
}

#[inline(always)]
fn connection_fact<const LENIENT: bool>(
    buf: &[u8],
    headers: &[Header],
    block: &Block,
) -> Result<Connection, Stop> {
    if LENIENT {
        return tolerant::connection(buf, headers).ok_or(Stop::Bad(MissReason::Nomination));
    }
    let h = &headers[usize::from(block.first[Class::Connection as usize])];
    match connection(buf, h, block.dup & Class::Connection.bit() != 0) {
        Connection::Other => bad(MissReason::Name),
        known => Ok(known),
    }
}

#[inline(always)]
fn scan<L: Lanes, const LENIENT: bool>(
    lanes: L,
    buf: &[u8],
    out: &mut ResponseHead,
) -> Result<(), Stop> {
    let (status, minor, reason, fields_start) = status_line::<LENIENT>(buf)?;
    let mut block = Block::new();
    scan_fields(lanes, buf, fields_start, &mut out.headers, &mut block)?;
    let headers = &out.headers[..usize::from(block.count)];
    let facts = &mut out.facts;
    let has = |class: Class| block.seen & class.bit() != 0;
    facts.content_length = None;
    if has(Class::ContentLength) && !(LENIENT && has(Class::TransferEncoding)) {
        facts.content_length = Some(content_length_fact::<LENIENT>(buf, headers, &block)?);
    }
    facts.connection = Connection::Absent;
    if has(Class::Connection) {
        facts.connection = connection_fact::<LENIENT>(buf, headers, &block)?;
    }
    facts.transfer_encoding = has(Class::TransferEncoding);
    facts.chunked_final = false;
    if facts.transfer_encoding {
        let last = headers.iter().rfind(|h| h.class == Class::TransferEncoding);
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

struct ScanHead<'a, const LENIENT: bool> {
    buf: &'a [u8],
    out: &'a mut ResponseHead,
}

impl<const LENIENT: bool> Job for ScanHead<'_, LENIENT> {
    type Out = Result<(), Stop>;

    #[inline(always)]
    fn run<L: Lanes>(self, lanes: L) -> Self::Out {
        scan::<L, LENIENT>(lanes, self.buf, self.out)
    }
}

fn scanned<const LENIENT: bool>(buf: &[u8], out: &mut ResponseHead) -> ScanResponse {
    if u32::try_from(buf.len()).is_err() {
        return ScanResponse::Slow;
    }
    match dispatch(ScanHead::<LENIENT> { buf, out }) {
        Ok(()) => ScanResponse::Head,
        Err(Stop::Partial) => ScanResponse::Partial,
        Err(Stop::Bad(_)) => ScanResponse::Slow,
    }
}

/// Scans the response head at the start of `buf`.
///
/// `Head` is only the common clean shape; `Slow` means "not that, or unsure": the classic parser decides, and it may
/// well accept the bytes. See the crate documentation for the shape.
#[must_use]
pub fn scan_response(buf: &[u8], out: &mut ResponseHead) -> ScanResponse {
    scanned::<false>(buf, out)
}

/// [`scan_response`] with the framing and connection rules of the classic client parse, for a head that is canonical
/// already (see [`canonicalize_response`](crate::canonicalize_response)): a status from 100 to 999, `Content-Length`
/// lines and list items that all agree, a `Content-Length` next to a `Transfer-Encoding` ignored, and any
/// `Connection` options as long as they are tokens that name no framing field and at most
/// [`MAX_NOMINATED`](crate::MAX_NOMINATED) of them name a field.
///
/// `Head` implies what it does for [`scan_response`], except that [`ResponseFacts::connection`] is what the classic
/// client takes from the first `Connection` line (`Close` when it has a `close` option, `KeepAlive` when it is exactly
/// `keep-alive`, `Absent` otherwise) and is never `Other`. [`ResponseHead::nominated`] lists the fields the options
/// name.
#[must_use]
pub fn scan_response_lenient(buf: &[u8], out: &mut ResponseHead) -> ScanResponse {
    scanned::<true>(buf, out)
}
