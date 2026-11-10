//! Response field sections: static references where the table has the field, literals otherwise, no Huffman coding,
//! no dynamic table (RFC 9204 section 4.5).
use http::{HeaderMap, StatusCode};
use oxiroute_h2::hpack::shared::integer_encode;

use super::static_table::{Found, find};

/// Static index of the first `:status` entry, used as the name reference for codes the table has no entry for.
const STATUS_NAME: usize = 24;

/// Appends the section prefix: Required Insert Count 0, Base 0.
#[inline]
pub fn prefix(out: &mut Vec<u8>) {
    out.extend_from_slice(&[0, 0]);
}

/// Static index of the `:status` entry for `code`.
#[inline]
const fn status_index(code: u16) -> Option<usize> {
    Some(match code {
        103 => 24,
        200 => 25,
        304 => 26,
        404 => 27,
        503 => 28,
        100 => 63,
        204 => 64,
        206 => 65,
        302 => 66,
        400 => 67,
        403 => 68,
        421 => 69,
        425 => 70,
        500 => 71,
        _ => return None,
    })
}

#[inline]
fn value(out: &mut Vec<u8>, value: &[u8]) {
    integer_encode(out, 0x00, 7, value.len());
    out.extend_from_slice(value);
}

/// Appends the `:status` line.
#[inline]
pub fn status(out: &mut Vec<u8>, status: StatusCode) {
    if let Some(index) = status_index(status.as_u16()) {
        integer_encode(out, 0xc0, 6, index);
    } else {
        integer_encode(out, 0x50, 4, STATUS_NAME);
        value(out, status.as_str().as_bytes());
    }
}

/// Appends one field line; `name` must be lowercase.
#[inline]
pub fn field(out: &mut Vec<u8>, name: &[u8], field_value: &[u8]) {
    match find(name, field_value) {
        Found::Exact(index) => integer_encode(out, 0xc0, 6, usize::from(index)),
        Found::Name(index) => {
            integer_encode(out, 0x50, 4, usize::from(index));
            value(out, field_value);
        }
        Found::None => {
            integer_encode(out, 0x20, 3, name.len());
            out.extend_from_slice(name);
            value(out, field_value);
        }
    }
}

/// Appends the lines of `headers`.
pub fn fields(out: &mut Vec<u8>, headers: &HeaderMap) {
    for (name, field_value) in headers {
        field(out, name.as_str().as_bytes(), field_value.as_bytes());
    }
}

/// Appends a complete response section.
pub fn response(out: &mut Vec<u8>, code: StatusCode, headers: &HeaderMap) {
    prefix(out);
    status(out, code);
    fields(out, headers);
}

/// Appends a complete trailer section.
pub fn trailers(out: &mut Vec<u8>, headers: &HeaderMap) {
    prefix(out);
    fields(out, headers);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qpack::decode::{Line, section};
    use crate::qpack::static_table::ENTRIES;

    #[test]
    fn every_status_index_points_at_its_entry() {
        for code in 100..600u16 {
            if let Some(index) = status_index(code) {
                assert_eq!(
                    ENTRIES[index],
                    (":status", StatusCode::from_u16(code).unwrap().as_str())
                );
            }
        }
    }

    #[test]
    fn statuses_use_the_table_when_it_has_them() {
        let mut out = Vec::new();
        status(&mut out, StatusCode::OK);
        assert_eq!(out, [0xc0 | 25]);
        out.clear();
        status(&mut out, StatusCode::IM_A_TEAPOT);
        assert_eq!(&out[..2], [0x5f, 24 - 15]);
        assert_eq!(&out[2..], [3, b'4', b'1', b'8']);
    }

    #[test]
    fn fields_pick_exact_name_reference_or_literal_name() {
        let mut out = Vec::new();
        field(&mut out, b"server", b"");
        assert_eq!(out, [0xff, 92 - 63]);
        out.clear();
        field(&mut out, b"content-type", b"x/y");
        assert_eq!(&out[..2], [0x50 | 15, 44 - 15]);
        out.clear();
        field(&mut out, b"x-a", b"v");
        assert_eq!(out, [0x23, b'x', b'-', b'a', 1, b'v']);
    }

    #[test]
    fn encoded_response_decodes_to_the_same_lines() {
        let mut headers = HeaderMap::new();
        headers.append("content-length", "1024".parse().unwrap());
        headers.append("set-cookie", "a=b".parse().unwrap());
        headers.append("set-cookie", "c=d".parse().unwrap());
        headers.append("x-long", "v".repeat(300).parse().unwrap());
        let mut out = Vec::new();
        response(&mut out, StatusCode::OK, &headers);
        let mut decoded = section(&out).unwrap();
        let mut count = 0;
        while let Some(line) = decoded.next_line().unwrap() {
            count += 1;
            if let Line::Literal { value, .. } = line {
                assert_eq!(value.bytes.len(), 300);
            }
        }
        assert_eq!(count, 5);
    }
}
