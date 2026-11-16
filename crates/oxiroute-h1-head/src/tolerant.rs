//! The framing and connection rules of the classic client parse for a response head, which the strict scan leaves to
//! it: `Content-Length` values that agree, and `Connection` options.

use crate::class::{Class, classify, ieq};
use crate::fields::Header;
use crate::request::Connection;
use crate::tables::{NAME, TOKEN};

/// The `Connection` options of a response that may name a field before the head is refused: the codec's edit list
/// holds a fixed number of removals.
pub const MAX_NOMINATED: usize = 4;

/// The comma separated options of a `Connection` value, without the whitespace around them and without empty ones.
pub(crate) fn options(value: &[u8]) -> impl Iterator<Item = &[u8]> {
    value
        .split(|&byte| byte == b',')
        .map(<[u8]>::trim_ascii)
        .filter(|option| !option.is_empty())
}

/// An option that is no field name: it says something about the connection itself.
pub(crate) fn is_reserved(option: &[u8]) -> bool {
    [&b"close"[..], b"keep-alive", b"upgrade"]
        .iter()
        .any(|reserved| option.eq_ignore_ascii_case(reserved))
}

fn digits(value: &[u8]) -> Option<u64> {
    if value.is_empty() {
        return None;
    }
    value.iter().try_fold(0u64, |length, &byte| {
        let digit = byte.is_ascii_digit().then(|| u64::from(byte - b'0'))?;
        length.checked_mul(10)?.checked_add(digit)
    })
}

/// The one length every `Content-Length` line and list item of the head states, `None` when one is not a number or
/// two differ.
pub(crate) fn reconciled_length(buf: &[u8], headers: &[Header]) -> Option<u64> {
    let mut agreed = None;
    for header in headers.iter().filter(|h| h.class == Class::ContentLength) {
        for item in header.value(buf).split(|&byte| byte == b',') {
            let length = digits(item.trim_ascii())?;
            if *agreed.get_or_insert(length) != length {
                return None;
            }
        }
    }
    agreed
}

fn names_a_field(option: &[u8]) -> bool {
    option
        .iter()
        .all(|&byte| NAME[usize::from(byte)] & TOKEN != 0)
        && !matches!(
            classify(option),
            Class::ContentLength | Class::TransferEncoding | Class::Host
        )
}

fn is_visible(value: &[u8]) -> bool {
    value
        .iter()
        .all(|&byte| byte == b'\t' || (b' '..=b'~').contains(&byte))
}

/// What the first `Connection` line says about keeping the connection.
fn keep_alive(first: &[u8]) -> Connection {
    if ieq::<10>(first, b"keep-alive") {
        Connection::KeepAlive
    } else if options(first).any(|option| option.eq_ignore_ascii_case(b"close")) {
        Connection::Close
    } else {
        Connection::Absent
    }
}

/// The keep-alive the `Connection` lines of the head state, `None` when a line is not visible ASCII, an option is not
/// a token or names a framing field or the host, or the options name more than [`MAX_NOMINATED`] fields.
pub(crate) fn connection(buf: &[u8], headers: &[Header]) -> Option<Connection> {
    let lines = headers
        .iter()
        .filter(|h| h.class == Class::Connection)
        .map(|h| h.value(buf));
    let first = lines.clone().next()?;
    let mut nominated = 0;
    for line in lines {
        if !is_visible(line) {
            return None;
        }
        for option in options(line).filter(|option| !is_reserved(option)) {
            if !names_a_field(option) {
                return None;
            }
            nominated += 1;
        }
    }
    (nominated <= MAX_NOMINATED).then(|| keep_alive(first))
}
