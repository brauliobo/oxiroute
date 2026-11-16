//! The head scanners of this crate written byte at a time, with no vector code and no shared state: the specification the
//! scanners are checked against for every verdict, miss reason, offset, header record and fact.

use oxiroute_cpu::{is_target_byte, is_tchar};
use oxiroute_h1_head::{
    Class, Connection, HOP_BY_HOP_REQUEST, HOP_BY_HOP_RESPONSE, Header, LINE_IS_CLEAN, MAX_HEADERS,
    MAX_NAME_LEN, MAX_RESPONSE_HEADERS, MissReason as M, NAME_HAS_UNDERSCORE, NAME_HAS_UPPER,
    RequestFacts, ResponseFacts, Span, classify,
};

const TAIL_IS_CLEAN: u8 = 8;
const MAX_METHOD_LEN: usize = 32;
const MAX_TARGET_LEN: usize = 65_534;
const TITLED: [Class; 12] = [
    Class::AcceptRanges,
    Class::Age,
    Class::CacheControl,
    Class::Connection,
    Class::ContentType,
    Class::ContentEncoding,
    Class::ContentLength,
    Class::Date,
    Class::TransferEncoding,
    Class::Host,
    Class::Server,
    Class::SetCookie,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    Partial,
    Bad(M),
}

type Step<T> = Result<T, Stop>;

fn bad<T>(reason: M) -> Step<T> {
    Err(Stop::Bad(reason))
}

#[derive(Debug, PartialEq, Eq)]
pub struct Request {
    pub method: Span,
    pub target: Span,
    pub line_len: u32,
    pub head_len: u32,
    pub headers: Vec<Header>,
    pub facts: RequestFacts,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub minor: u8,
    pub reason: Span,
    pub line_len: u32,
    pub head_len: u32,
    pub headers: Vec<Header>,
    pub facts: ResponseFacts,
}

struct Block {
    headers: Vec<Header>,
    head_len: u32,
}

impl Block {
    fn first(&self, class: Class) -> Option<&Header> {
        self.headers.iter().find(|h| h.class == class)
    }

    fn count(&self, class: Class) -> usize {
        self.headers.iter().filter(|h| h.class == class).count()
    }

    fn present(&self) -> u32 {
        self.headers.iter().fold(0, |seen, h| {
            seen | if h.class == Class::Other {
                0
            } else {
                h.class.bit()
            }
        })
    }

    fn name_flags(&self) -> u8 {
        self.headers.iter().fold(0, |flags, h| {
            flags | (h.flags & (NAME_HAS_UPPER | NAME_HAS_UNDERSCORE))
        })
    }

    fn duplicate_names(&self, buf: &[u8]) -> bool {
        self.headers.iter().enumerate().any(|(i, a)| {
            self.headers[..i]
                .iter()
                .any(|b| a.name(buf).eq_ignore_ascii_case(b.name(buf)))
        })
    }

    fn index_of(&self, class: Class) -> u8 {
        self.headers
            .iter()
            .position(|h| h.class == class)
            .map_or(0, |i| i as u8)
    }
}

fn crlf(buf: &[u8], i: usize) -> Step<usize> {
    match (buf.get(i), buf.get(i + 1)) {
        (Some(b'\r'), Some(b'\n')) => Ok(i + 2),
        (Some(b'\r'), Some(_)) => bad(M::BareCr),
        (Some(b'\r'), None) | (None, _) => Err(Stop::Partial),
        (Some(b'\n'), _) => bad(M::BareLf),
        (Some(_), _) => bad(M::Version),
    }
}

fn is_ows(b: u8) -> bool {
    b == b' ' || b == b'\t'
}

fn value_end(buf: &[u8], start: usize) -> Step<usize> {
    let mut end = start;
    loop {
        match buf.get(end) {
            Some(b'\t') => end += 1,
            Some(b'\r') => {
                return match buf.get(end + 1) {
                    Some(b'\n') => Ok(end),
                    Some(_) => bad(M::BareCr),
                    None => Err(Stop::Partial),
                };
            }
            Some(b'\n') => return bad(M::BareLf),
            Some(&c) if c < 0x20 || c == 0x7f => return bad(M::Control),
            Some(_) => end += 1,
            None => return Err(Stop::Partial),
        }
    }
}

fn title_matches(class: Class, name: &[u8]) -> bool {
    TITLED.contains(&class) && class.titled() == Some(name)
}

fn flag(on: bool, bit: u8) -> u8 {
    u8::from(on) * bit
}

fn field_line(buf: &[u8], i: usize) -> Step<(Header, usize)> {
    let len = buf[i..].iter().take_while(|&&b| is_tchar(b)).count();
    let name_end = i + len;
    match buf.get(name_end) {
        Some(b':') if len > 0 => {}
        Some(b' ' | b'\t') if len == 0 => return bad(M::Folding),
        Some(b' ' | b'\t') => return bad(M::SpaceBeforeColon),
        Some(_) => return bad(M::Name),
        None => return Err(Stop::Partial),
    }
    if len > MAX_NAME_LEN {
        return bad(M::NameTooLong);
    }
    let mut value_start = name_end + 1;
    while buf.get(value_start).copied().is_some_and(is_ows) {
        value_start += 1;
    }
    let end = value_end(buf, value_start)?;
    let mut value_stop = end;
    while value_stop > value_start && is_ows(buf[value_stop - 1]) {
        value_stop -= 1;
    }
    let name = &buf[i..name_end];
    let class = classify(name);
    let upper = name.iter().any(u8::is_ascii_uppercase);
    let tail_clean = value_start == name_end + 2 && value_stop == end && buf[name_end + 1] == b' ';
    let name_clean = if TITLED.contains(&class) {
        upper && title_matches(class, name)
    } else {
        !upper
    };
    let flags = flag(upper, NAME_HAS_UPPER)
        | flag(name.contains(&b'_'), NAME_HAS_UNDERSCORE)
        | flag(tail_clean && name_clean, LINE_IS_CLEAN)
        | flag(tail_clean, TAIL_IS_CLEAN);
    let header = Header {
        name_start: i as u32,
        value_start: value_start as u32,
        value_len: (value_stop - value_start) as u32,
        name_len: len as u8,
        class,
        flags,
    };
    Ok((header, end + 2))
}

fn fields(buf: &[u8], start: usize, limit: usize) -> Step<Block> {
    let mut headers: Vec<Header> = Vec::new();
    let mut i = start;
    loop {
        match buf.get(i) {
            None => return Err(Stop::Partial),
            Some(b'\r') => {
                let head_len = crlf(buf, i)? as u32;
                return Ok(Block { headers, head_len });
            }
            Some(b'\n') => return bad(M::BareLf),
            Some(_) => {}
        }
        let (header, next) = field_line(buf, i)?;
        if headers.len() == limit {
            return bad(M::TooManyHeaders);
        }
        headers.push(header);
        i = next;
    }
}

fn content_length(value: &[u8]) -> Option<u64> {
    if value.is_empty() || value.len() > 18 || !value.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(value).ok()?.parse().ok()
}

fn chunked_final(value: &[u8]) -> bool {
    let last = value.rsplit(|&c| c == b',').next().unwrap_or(value);
    last.trim_ascii().eq_ignore_ascii_case(b"chunked")
}

fn connection_of(buf: &[u8], block: &Block) -> Connection {
    let Some(first) = block.first(Class::Connection) else {
        return Connection::Absent;
    };
    let value = first.value(buf);
    if block.count(Class::Connection) > 1 {
        Connection::Other
    } else if value.eq_ignore_ascii_case(b"keep-alive") {
        Connection::KeepAlive
    } else if value.eq_ignore_ascii_case(b"close") {
        Connection::Close
    } else {
        Connection::Other
    }
}

fn length_of(buf: &[u8], block: &Block) -> Step<Option<u64>> {
    let Some(first) = block.first(Class::ContentLength) else {
        return Ok(None);
    };
    let length =
        content_length(first.value(buf)).filter(|_| block.count(Class::ContentLength) == 1);
    length.map(Some).ok_or(Stop::Bad(M::ContentLength))
}

fn last_te_chunked(buf: &[u8], block: &Block) -> bool {
    block
        .headers
        .iter()
        .rfind(|h| h.class == Class::TransferEncoding)
        .is_some_and(|h| chunked_final(h.value(buf)))
}

pub fn request(buf: &[u8]) -> Step<Request> {
    if u32::try_from(buf.len()).is_err() {
        return bad(M::TooLarge);
    }
    let method = buf.iter().take_while(|&&b| is_tchar(b)).count();
    match buf.get(method) {
        Some(b' ') if method > 0 && method <= MAX_METHOD_LEN => {}
        _ if method > MAX_METHOD_LEN => return bad(M::MethodTooLong),
        Some(_) => return bad(M::Method),
        None => return Err(Stop::Partial),
    }
    let target_start = method + 1;
    match buf.get(target_start) {
        Some(b'/') => {}
        Some(_) => return bad(M::Target),
        None => return Err(Stop::Partial),
    }
    let target_end = target_start
        + 1
        + buf[target_start + 1..]
            .iter()
            .take_while(|&&b| is_target_byte(b))
            .count();
    if target_end - target_start > MAX_TARGET_LEN {
        return bad(M::TargetTooLong);
    }
    match buf.get(target_end) {
        Some(b' ') => {}
        Some(_) => return bad(M::Target),
        None => return Err(Stop::Partial),
    }
    let version = target_end + 1;
    let fields_start = version_end(buf, version)?;
    let block = fields(buf, fields_start, MAX_HEADERS)?;
    let facts = request_facts(buf, &block)?;
    Ok(Request {
        method: Span {
            start: 0,
            len: method as u32,
        },
        target: Span {
            start: target_start as u32,
            len: (target_end - target_start) as u32,
        },
        line_len: fields_start as u32,
        head_len: block.head_len,
        headers: block.headers,
        facts,
    })
}

fn version_end(buf: &[u8], start: usize) -> Step<usize> {
    let expected = b"HTTP/1.1";
    let rest = &buf[start.min(buf.len())..];
    let shown = &rest[..rest.len().min(8)];
    if shown.len() < 8 {
        return if expected.starts_with(shown) {
            Err(Stop::Partial)
        } else {
            bad(M::Version)
        };
    }
    if shown != expected {
        return bad(if shown == b"HTTP/1.0" {
            M::Version10
        } else {
            M::Version
        });
    }
    match (buf.get(start + 8), buf.get(start + 9)) {
        (Some(b'\r'), Some(b'\n')) => Ok(start + 10),
        (Some(b'\r'), None) | (None, _) => Err(Stop::Partial),
        (Some(b'\n'), _) => bad(M::BareLf),
        (Some(b'\r'), Some(_)) => bad(M::BareCr),
        (Some(_), _) => bad(M::Version),
    }
}

fn request_facts(buf: &[u8], block: &Block) -> Step<RequestFacts> {
    let present = block.present();
    let has = |class: Class| present & class.bit() != 0;
    if !has(Class::Host) {
        return bad(M::HostMissing);
    }
    if block.count(Class::Host) > 1 {
        return bad(M::HostDuplicate);
    }
    let mut content = length_of(buf, block)?;
    let connection = connection_of(buf, block);
    let transfer_encoding = has(Class::TransferEncoding);
    if transfer_encoding {
        if !last_te_chunked(buf, block) {
            return bad(M::TransferEncoding);
        }
        content = None;
    }
    Ok(RequestFacts {
        host: block.index_of(Class::Host),
        content_length: content,
        transfer_encoding,
        upgrade: has(Class::Upgrade),
        expect: has(Class::Expect),
        connection,
        hop_by_hop: ((present >> Class::KeepAlive as u32) as u16) & HOP_BY_HOP_REQUEST,
        request_id_count: block.count(Class::XRequestId).min(255) as u8,
        request_id: block.index_of(Class::XRequestId),
        has_underscore: block.name_flags() & NAME_HAS_UNDERSCORE != 0,
        has_uppercase: block.name_flags() & NAME_HAS_UPPER != 0,
        duplicate_names: block.duplicate_names(buf),
        present,
    })
}

fn status_line(buf: &[u8]) -> Step<(u16, u8, Span, usize)> {
    const SHAPE: &[u8; 13] = b"HTTP/1.? 000 ";
    for (i, &c) in buf.iter().take(13).enumerate() {
        let fits = match i {
            7 => c == b'0' || c == b'1',
            9..=11 => c.is_ascii_digit(),
            _ => c == SHAPE[i],
        };
        if !fits {
            return bad(M::Version);
        }
    }
    if buf.len() < 13 {
        return Err(Stop::Partial);
    }
    let status: u16 = std::str::from_utf8(&buf[9..12]).unwrap().parse().unwrap();
    if !(100..=599).contains(&status) {
        return bad(M::Version);
    }
    let mut end = 13;
    while buf
        .get(end)
        .is_some_and(|&c| c == b'\t' || (b' '..=b'~').contains(&c))
    {
        end += 1;
    }
    match (buf.get(end), buf.get(end + 1)) {
        (Some(b'\r'), Some(b'\n')) => {}
        (Some(b'\r'), None) | (None, _) => return Err(Stop::Partial),
        _ => return bad(M::Control),
    }
    let span = Span {
        start: 13,
        len: (end - 13) as u32,
    };
    Ok((status, buf[7] - b'0', span, end + 2))
}

/// `Err(Stop::Bad(_))` is what the real scanner calls `Slow`.
pub fn response(buf: &[u8]) -> Step<Response> {
    if u32::try_from(buf.len()).is_err() {
        return bad(M::TooLarge);
    }
    let (status, minor, reason, fields_start) = status_line(buf)?;
    let block = fields(buf, fields_start, MAX_RESPONSE_HEADERS)?;
    let present = block.present();
    let has = |class: Class| present & class.bit() != 0;
    let mut content = length_of(buf, &block)?;
    let connection = connection_of(buf, &block);
    if connection == Connection::Other {
        return bad(M::Name);
    }
    let transfer_encoding = has(Class::TransferEncoding);
    let chunked = transfer_encoding && last_te_chunked(buf, &block);
    if transfer_encoding {
        content = None;
    }
    let facts = ResponseFacts {
        content_length: content,
        transfer_encoding,
        chunked_final: chunked,
        connection,
        upgrade: has(Class::Upgrade),
        hop_by_hop: ((present >> Class::KeepAlive as u32) as u16) & HOP_BY_HOP_RESPONSE,
        request_id: has(Class::XRequestId),
        upstream_request_id: has(Class::XUpstreamRequestId),
        informational: status < 200,
        has_underscore: block.name_flags() & NAME_HAS_UNDERSCORE != 0,
        has_uppercase: block.name_flags() & NAME_HAS_UPPER != 0,
        duplicate_names: block.duplicate_names(buf),
        present,
    };
    Ok(Response {
        status,
        minor,
        reason,
        line_len: fields_start as u32,
        head_len: block.head_len,
        headers: block.headers,
        facts,
    })
}
