//! Request field validity (RFC 9113 section 8) on bytes, without building `http` types. The tables and rules are
//! the ones `http` applies to header names, header values and the path of a URI, so a field accepted here converts
//! to an `http` value without failing (checked against `http` itself by the unit tests).

/// Declared length, `u64::MAX` when absent.
pub const UNDECLARED: u64 = u64::MAX;

const MAX_NAME: usize = (1 << 16) - 1;
const MAX_PATH: usize = (1 << 16) - 2;

const fn name_bytes() -> [bool; 256] {
    let mut table = [false; 256];
    let mut b = 0;
    while b < 256 {
        table[b] = matches!(
            b as u8,
            b'!' | b'"' | b'#' | b'$' | b'%' | b'&' | b'\'' | b'*' | b'+' | b'-' | b'.' | b'^'
            | b'_' | b'`' | b'|' | b'~' | b'0'..=b'9' | b'a'..=b'z'
        );
        b += 1;
    }
    table
}

const NAME: [bool; 256] = name_bytes();

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Valid,
    Query,
    Fragment,
    High,
    Invalid,
}

const fn path_classes(query: bool) -> [Class; 256] {
    let mut table = [Class::Invalid; 256];
    let mut b = 0;
    while b < 256 {
        table[b] = match b as u8 {
            b'#' => Class::Fragment,
            b'?' if !query => Class::Query,
            0x21 | 0x24..=0x3B | 0x3D | 0x3F..=0x7E if query => Class::Valid,
            0x21 | 0x24..=0x3B | 0x3D | 0x40..=0x5F | 0x61..=0x7A | 0x7C | 0x7E => Class::Valid,
            b'"' | b'{' | b'}' if !query => Class::Valid,
            0x80..=0xFF => Class::High,
            _ => Class::Invalid,
        };
        b += 1;
    }
    table
}

const PATH: [Class; 256] = path_classes(false);
const QUERY: [Class; 256] = path_classes(true);

/// A lowercase `http` header name (and so not a pseudo-header).
pub fn name(name: &[u8]) -> bool {
    let bad = name
        .iter()
        .fold(0u8, |bad, &b| bad | u8::from(!NAME[usize::from(b)]));
    (1..=MAX_NAME).contains(&name.len()) && bad == 0
}

pub fn value(value: &[u8]) -> bool {
    let bad = value.iter().fold(0u8, |bad, &b| {
        bad | u8::from(!((b >= 32 && b != 127) || b == b'\t'))
    });
    bad == 0
}

/// The `:path` as `http::uri::PathAndQuery` reads it: the part before a fragment (`/` when nothing is left, which
/// is what `PathAndQuery::as_str` shows for it), `None` when it is not valid.
pub fn path(path: &[u8]) -> Option<&[u8]> {
    if path.is_empty() || path.len() > MAX_PATH {
        return None;
    }
    if path == b"*" {
        return Some(path);
    }
    if !matches!(path[0], b'/' | b'?' | b'#') {
        return None;
    }
    let (mut table, mut high, mut end) = (&PATH, false, path.len());
    for (i, &b) in path.iter().enumerate() {
        match table[usize::from(b)] {
            Class::Valid => {}
            Class::Query => table = &QUERY,
            Class::High => high = true,
            Class::Fragment => {
                end = i;
                break;
            }
            Class::Invalid => return None,
        }
    }
    let path = &path[..end];
    if path.is_empty() {
        return Some(b"/");
    }
    (!high || std::str::from_utf8(path).is_ok()).then_some(path)
}

/// `http` and `https` need no further check; anything else goes through `http`'s own scheme parser.
pub fn scheme(scheme: &[u8]) -> bool {
    matches!(scheme, b"http" | b"https") || http::uri::Scheme::try_from(scheme).is_ok()
}

fn is_connection_specific(name: &[u8]) -> bool {
    matches!(
        name,
        b"connection" | b"transfer-encoding" | b"upgrade" | b"keep-alive" | b"proxy-connection"
    )
}

/// Digits only, at most 19 of them (so the value cannot overflow).
fn parse_u64(src: &[u8]) -> Option<u64> {
    if src.is_empty() || src.len() > 19 || !src.iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some(src.iter().fold(0, |n, d| n * 10 + u64::from(d - b'0')))
}

/// A regular field of a request or trailer block: valid syntax, no connection-specific field, `te` only
/// `trailers`, and every `content-length` a number equal to the first one (kept in `content_length`).
pub fn field(key: &[u8], val: &[u8], content_length: &mut Option<u64>) -> bool {
    name(key)
        && value(val)
        && !is_connection_specific(key)
        && (key != b"te" || val == b"trailers")
        && (key != b"content-length"
            || parse_u64(val).is_some_and(|n| *content_length.get_or_insert(n) == n))
}

#[cfg(test)]
mod tests;
