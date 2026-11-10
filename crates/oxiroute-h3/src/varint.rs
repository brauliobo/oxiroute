//! QUIC variable-length integers (RFC 9000 section 16).

/// Largest value an integer can carry.
pub const MAX: u64 = (1 << 62) - 1;

/// Bytes of the encoding that starts with `first`.
#[inline]
#[must_use]
pub const fn len_from_first(first: u8) -> usize {
    1 << (first >> 6)
}

/// Reads one integer from the front of `src`, or `None` when the input ends inside it.
#[inline]
pub fn decode(src: &mut &[u8]) -> Option<u64> {
    let first = *src.first()?;
    let n = len_from_first(first);
    let (head, rest) = src.split_at_checked(n)?;
    let value = head[1..]
        .iter()
        .fold(u64::from(first & 0x3f), |acc, &b| acc << 8 | u64::from(b));
    *src = rest;
    Some(value)
}

/// Bytes the shortest encoding of `value` takes.
#[inline]
#[must_use]
pub const fn size(value: u64) -> usize {
    match value {
        0..=63 => 1,
        64..=16_383 => 2,
        16_384..=1_073_741_823 => 4,
        _ => 8,
    }
}

/// Appends the shortest encoding of `value`, which must not exceed [`MAX`].
#[inline]
pub fn encode(out: &mut Vec<u8>, value: u64) {
    debug_assert!(value <= MAX);
    match size(value) {
        1 => out.push(value as u8),
        2 => out.extend_from_slice(&(value as u16 | 0x4000).to_be_bytes()),
        4 => out.extend_from_slice(&(value as u32 | 0x8000_0000).to_be_bytes()),
        _ => out.extend_from_slice(&(value | 0xc000_0000_0000_0000).to_be_bytes()),
    }
}

/// The shortest encoding of `value` in a fixed array, with its length.
#[inline]
#[must_use]
pub fn encode_array(value: u64) -> ([u8; 8], usize) {
    debug_assert!(value <= MAX);
    let n = size(value);
    let mut out = value.to_be_bytes();
    out.rotate_left(8 - n);
    out[0] |= match n {
        1 => 0x00,
        2 => 0x40,
        4 => 0x80,
        _ => 0xc0,
    };
    (out, n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(value: u64) -> Vec<u8> {
        let mut out = Vec::new();
        encode(&mut out, value);
        assert_eq!(out.len(), size(value));
        let mut src = &out[..];
        assert_eq!(decode(&mut src), Some(value));
        assert!(src.is_empty());
        out
    }

    #[test]
    fn rfc_9000_appendix_a_examples() {
        let mut src: &[u8] = &[0xc2, 0x19, 0x7c, 0x5e, 0xff, 0x14, 0xe8, 0x8c];
        assert_eq!(decode(&mut src), Some(151_288_809_941_952_652));
        let mut src: &[u8] = &[0x9d, 0x7f, 0x3e, 0x7d];
        assert_eq!(decode(&mut src), Some(494_878_333));
        let mut src: &[u8] = &[0x7b, 0xbd];
        assert_eq!(decode(&mut src), Some(15_293));
        let mut src: &[u8] = &[0x25];
        assert_eq!(decode(&mut src), Some(37));
        let mut src: &[u8] = &[0x40, 0x25];
        assert_eq!(
            decode(&mut src),
            Some(37),
            "non-minimal encodings are valid"
        );
    }

    #[test]
    fn round_trips_around_every_size_boundary() {
        for base in [0, 63, 16_383, 1_073_741_823, MAX] {
            for value in base.saturating_sub(1)..=(base + 1).min(MAX) {
                roundtrip(value);
            }
        }
        assert_eq!(roundtrip(63), [0x3f]);
        assert_eq!(roundtrip(64), [0x40, 0x40]);
    }

    #[test]
    fn array_encoding_matches_vec_encoding() {
        for value in [
            0,
            1,
            63,
            64,
            16_383,
            16_384,
            1_073_741_823,
            1_073_741_824,
            MAX,
        ] {
            let (array, n) = encode_array(value);
            assert_eq!(&array[..n], roundtrip(value));
        }
    }

    #[test]
    fn truncated_input_yields_none_and_consumes_nothing() {
        let full = roundtrip(MAX);
        for cut in 0..full.len() {
            let mut src = &full[..cut];
            assert_eq!(decode(&mut src), None);
            assert_eq!(src.len(), cut);
        }
    }
}
