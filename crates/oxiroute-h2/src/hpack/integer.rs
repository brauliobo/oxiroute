//! Prefix integers (RFC 7541 section 5.1).

/// Continuation octets accepted after the prefix; five carry 35 bits, more than any length or index needs.
const MAX_CONTINUATIONS: u32 = 5;

/// Reads an integer with an `prefix`-bit prefix from the front of `src`, or `None` when the input ends inside it or
/// the value does not fit 32 bits.
#[inline]
pub(super) fn decode(src: &mut &[u8], prefix: u32) -> Option<usize> {
    let (&first, rest) = src.split_first()?;
    let mask = (1u8 << prefix) - 1;
    *src = rest;
    let head = first & mask;
    if head < mask {
        return Some(usize::from(head));
    }
    decode_continuation(src, u64::from(mask))
}

#[cold]
fn decode_continuation(src: &mut &[u8], mut value: u64) -> Option<usize> {
    for step in 0..MAX_CONTINUATIONS {
        let (&byte, rest) = src.split_first()?;
        *src = rest;
        value += u64::from(byte & 0x7f) << (7 * step);
        if byte & 0x80 == 0 {
            return u32::try_from(value).ok().map(|v| v as usize);
        }
    }
    None
}

/// Appends `value` with a `prefix`-bit prefix whose high bits are `first`.
#[inline]
pub(super) fn encode(out: &mut Vec<u8>, first: u8, prefix: u32, value: usize) {
    let mask = (1usize << prefix) - 1;
    if value < mask {
        out.push(first | value as u8);
    } else {
        encode_continuation(out, first | mask as u8, value - mask);
    }
}

fn encode_continuation(out: &mut Vec<u8>, first: u8, mut rest: usize) {
    out.push(first);
    while rest >= 128 {
        out.push((rest & 0x7f) as u8 | 0x80);
        rest >>= 7;
    }
    out.push(rest as u8);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(prefix: u32, value: usize) -> Option<usize> {
        let mut out = Vec::new();
        encode(&mut out, 0, prefix, value);
        let mut src = &out[..];
        let decoded = decode(&mut src, prefix)?;
        assert!(src.is_empty());
        Some(decoded)
    }

    #[test]
    fn rfc_c1_examples() {
        let mut out = Vec::new();
        encode(&mut out, 0, 5, 10);
        assert_eq!(out, [0b0000_1010]);
        out.clear();
        encode(&mut out, 0, 5, 1337);
        assert_eq!(out, [0b0001_1111, 0b1001_1010, 0b0000_1010]);
        out.clear();
        encode(&mut out, 0, 8, 42);
        assert_eq!(out, [42]);
    }

    #[test]
    fn round_trips_around_every_boundary() {
        for prefix in 1..=7u32 {
            let mask = (1usize << prefix) - 1;
            for base in [
                0,
                mask,
                mask + 127,
                mask + 128,
                16_383,
                1 << 21,
                1 << 28,
                u32::MAX as usize - 1,
            ] {
                for value in base.saturating_sub(2)..=base + 2 {
                    if u32::try_from(value).is_ok() {
                        assert_eq!(
                            roundtrip(prefix, value),
                            Some(value),
                            "prefix {prefix} value {value}"
                        );
                    }
                }
            }
        }
        assert_eq!(roundtrip(7, u32::MAX as usize), Some(u32::MAX as usize));
    }

    #[test]
    fn values_past_32_bits_are_rejected() {
        assert_eq!(roundtrip(7, u32::MAX as usize + 1), None);
        let mut src: &[u8] = &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f];
        assert_eq!(decode(&mut src, 7), None);
    }

    #[test]
    fn endless_continuations_are_rejected() {
        let mut src: &[u8] = &[0xff, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x00];
        assert_eq!(decode(&mut src, 7), None);
    }

    #[test]
    fn truncated_integers_are_rejected() {
        for input in [&[][..], &[0x7f], &[0x7f, 0x80], &[0xff, 0xff]] {
            let mut src = input;
            assert_eq!(decode(&mut src, 7), None);
        }
    }
}
