//! Equality of short byte strings without a call into `memcmp`: names and values here are a few dozen octets.

/// `a == b` with word-sized loads; the last word overlaps the previous one instead of looping over a remainder.
#[inline]
pub(super) fn eq(a: &[u8], b: &[u8]) -> bool {
    let len = a.len();
    if len != b.len() {
        return false;
    }
    if len >= 8 {
        let mut at = 0;
        while at + 8 < len {
            if word(a, at) != word(b, at) {
                return false;
            }
            at += 8;
        }
        word(a, len - 8) == word(b, len - 8)
    } else if len >= 4 {
        half(a, 0) == half(b, 0) && half(a, len - 4) == half(b, len - 4)
    } else {
        a.iter().zip(b).all(|(x, y)| x == y)
    }
}

#[inline]
fn word(s: &[u8], at: usize) -> u64 {
    u64::from_ne_bytes(s[at..at + 8].try_into().unwrap_or_default())
}

#[inline]
fn half(s: &[u8], at: usize) -> u32 {
    u32::from_ne_bytes(s[at..at + 4].try_into().unwrap_or_default())
}

#[cfg(test)]
mod tests {
    #[test]
    fn agrees_with_slice_equality() {
        let base: Vec<u8> = (0..40).collect();
        for len in 0..40 {
            for flip in 0..=len {
                let mut other = base[..len].to_vec();
                if flip < len {
                    other[flip] ^= 1;
                }
                assert_eq!(
                    super::eq(&base[..len], &other),
                    flip == len,
                    "len {len} flip {flip}"
                );
            }
        }
        assert!(!super::eq(b"abc", b"abcd"));
    }
}
