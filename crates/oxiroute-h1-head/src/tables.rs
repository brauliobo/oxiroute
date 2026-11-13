//! Byte classes of the HTTP/1 grammar the scanners accept.

use oxiroute_cpu::is_tchar;

/// Index of the first byte below 0x20 or DEL at or after `i`, or the buffer length. A tab is reported like any
/// other control byte; the caller decides what it means.
pub(crate) use oxiroute_cpu::control_at;

/// Bit set for a token byte (`tchar` of RFC 9110).
pub(crate) const TOKEN: u8 = 1;
/// Bit set for an ASCII capital.
pub(crate) const UPPER: u8 = 2;
/// Bit set for an underscore.
pub(crate) const UNDERSCORE: u8 = 4;

const fn name_table() -> [u8; 256] {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        let b = i as u8;
        if is_tchar(b) {
            t[i] = TOKEN
                | if b.is_ascii_uppercase() { UPPER } else { 0 }
                | if b == b'_' { UNDERSCORE } else { 0 };
        }
        i += 1;
    }
    t
}

/// Token bytes with their class bits.
pub(crate) static NAME: [u8; 256] = name_table();

/// Bytes `http::Uri` accepts both in the path and in the query, so a target made of them is stored and written back
/// unchanged. Stricter than httparse on purpose: no `"`, `` ` ``, `#`, space, control or high byte.
const fn target_table() -> [bool; 256] {
    let mut t = [false; 256];
    let mut i = 0;
    while i < 256 {
        let b = i as u8;
        t[i] = matches!(b, 0x21 | 0x24..=0x3b | 0x3d | 0x3f..=0x5f | 0x61..=0x7a | 0x7b..=0x7e);
        i += 1;
    }
    t
}

pub(crate) static TARGET: [bool; 256] = target_table();

/// Whether `bytes` are valid `HeaderValue` contents: no control byte but a tab, no DEL.
#[must_use]
pub fn is_header_value(bytes: &[u8]) -> bool {
    let mut i = control_at(bytes, 0);
    while i < bytes.len() {
        if bytes[i] != b'\t' {
            return false;
        }
        i = control_at(bytes, i + 1);
    }
    true
}
