//! Byte classes of the HTTP/1 grammar the scanners accept.

use oxiroute_cpu::is_tchar;

/// Bit set for a token byte (`tchar` of RFC 9110).
pub(crate) const TOKEN: u8 = 1;
/// Bit set for an ASCII capital.
pub(crate) const UPPER: u8 = 2;

const fn name_table() -> [u8; 256] {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        let b = i as u8;
        if is_tchar(b) {
            t[i] = TOKEN | if b.is_ascii_uppercase() { UPPER } else { 0 };
        }
        i += 1;
    }
    t
}

/// Token bytes with their class bits.
pub(crate) static NAME: [u8; 256] = name_table();

/// Whether `bytes` are valid `HeaderValue` contents: no control byte but a tab, no DEL.
#[must_use]
pub fn is_header_value(bytes: &[u8]) -> bool {
    oxiroute_cpu::header_value_end(bytes, 0) == bytes.len()
}
