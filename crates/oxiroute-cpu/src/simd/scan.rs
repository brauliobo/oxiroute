//! Const byte classes specialize each kernel without a per-vector predicate branch.

use super::{live, load128, load256, load512, load512_masked};
use crate::Level;
use crate::scan::{ANY3, HEADER_CONTROL, JSON_ESCAPE, OUTSIDE_RANGE, scalar_at};
use core::arch::x86_64::*;

pub(crate) fn at<const KIND: u8>(level: Level, buf: &[u8], from: usize, args: [u8; 3]) -> usize {
    // SAFETY: callers supply a level clamped to the CPU and OS features.
    unsafe {
        match level {
            Level::Scalar => scalar_at::<KIND>(buf, from, args),
            Level::Sse2 | Level::Sse42 => at_sse42::<KIND>(buf, from, args),
            Level::Avx2 => at_avx2::<KIND>(buf, from, args),
            Level::Avx512 => at_avx512::<KIND>(buf, from, args),
        }
    }
}

#[inline]
#[target_feature(enable = "sse2")]
fn mask128<const KIND: u8>(x: __m128i, args: [u8; 3]) -> u64 {
    let eq = |b: u8| _mm_cmpeq_epi8(x, _mm_set1_epi8(b.cast_signed()));
    let below = _mm_cmpeq_epi8(_mm_min_epu8(x, _mm_set1_epi8(0x1f)), x);
    let mask = match KIND {
        ANY3 => _mm_or_si128(eq(args[0]), _mm_or_si128(eq(args[1]), eq(args[2]))),
        OUTSIDE_RANGE => {
            let low = _mm_cmpeq_epi8(_mm_max_epu8(x, _mm_set1_epi8(args[0].cast_signed())), x);
            let high = _mm_cmpeq_epi8(_mm_min_epu8(x, _mm_set1_epi8(args[1].cast_signed())), x);
            _mm_cmpeq_epi8(_mm_and_si128(low, high), _mm_setzero_si128())
        }
        JSON_ESCAPE => _mm_or_si128(below, _mm_or_si128(eq(b'"'), eq(b'\\'))),
        HEADER_CONTROL => {
            let controls = _mm_movemask_epi8(_mm_or_si128(below, eq(0x7f))).cast_unsigned();
            return u64::from(if controls == 0 {
                0
            } else {
                controls & !_mm_movemask_epi8(eq(b'\t')).cast_unsigned()
            });
        }
        _ => unreachable!(),
    };
    u64::from(_mm_movemask_epi8(mask).cast_unsigned())
}

#[inline]
#[target_feature(enable = "avx2")]
fn mask256<const KIND: u8>(x: __m256i, args: [u8; 3]) -> u64 {
    let eq = |b: u8| _mm256_cmpeq_epi8(x, _mm256_set1_epi8(b.cast_signed()));
    let below = _mm256_cmpeq_epi8(_mm256_min_epu8(x, _mm256_set1_epi8(0x1f)), x);
    let mask = match KIND {
        ANY3 => _mm256_or_si256(eq(args[0]), _mm256_or_si256(eq(args[1]), eq(args[2]))),
        OUTSIDE_RANGE => {
            let low = _mm256_cmpeq_epi8(
                _mm256_max_epu8(x, _mm256_set1_epi8(args[0].cast_signed())),
                x,
            );
            let high = _mm256_cmpeq_epi8(
                _mm256_min_epu8(x, _mm256_set1_epi8(args[1].cast_signed())),
                x,
            );
            _mm256_cmpeq_epi8(_mm256_and_si256(low, high), _mm256_setzero_si256())
        }
        JSON_ESCAPE => _mm256_or_si256(below, _mm256_or_si256(eq(b'"'), eq(b'\\'))),
        HEADER_CONTROL => {
            let controls = _mm256_movemask_epi8(_mm256_or_si256(below, eq(0x7f))).cast_unsigned();
            return u64::from(if controls == 0 {
                0
            } else {
                controls & !_mm256_movemask_epi8(eq(b'\t')).cast_unsigned()
            });
        }
        _ => unreachable!(),
    };
    u64::from(_mm256_movemask_epi8(mask).cast_unsigned())
}

#[inline]
#[target_feature(enable = "avx512bw")]
fn mask512<const KIND: u8>(x: __m512i, args: [u8; 3]) -> u64 {
    let eq = |b: u8| _mm512_cmpeq_epi8_mask(x, _mm512_set1_epi8(b.cast_signed()));
    let below = _mm512_cmple_epu8_mask(x, _mm512_set1_epi8(0x1f));
    match KIND {
        ANY3 => eq(args[0]) | eq(args[1]) | eq(args[2]),
        OUTSIDE_RANGE => {
            _mm512_cmplt_epu8_mask(x, _mm512_set1_epi8(args[0].cast_signed()))
                | _mm512_cmpgt_epu8_mask(x, _mm512_set1_epi8(args[1].cast_signed()))
        }
        JSON_ESCAPE => below | eq(b'"') | eq(b'\\'),
        HEADER_CONTROL => {
            let del = eq(0x7f);
            // Separate masks let the common path use KORTEST without first joining them.
            if _kortestz_mask64_u8(below, del) != 0 {
                0
            } else {
                (below & !eq(b'\t')) | del
            }
        }
        _ => unreachable!(),
    }
}

#[target_feature(enable = "sse2")]
fn at_sse42<const KIND: u8>(buf: &[u8], mut from: usize, args: [u8; 3]) -> usize {
    let Some(rest) = buf.get(from..) else {
        return from;
    };
    for bytes in rest.as_chunks::<16>().0 {
        let found = mask128::<KIND>(load128(bytes), args);
        if found != 0 {
            return from + found.trailing_zeros() as usize;
        }
        from += 16;
    }
    scalar_at::<KIND>(buf, from, args)
}

#[target_feature(enable = "avx2")]
fn at_avx2<const KIND: u8>(buf: &[u8], mut from: usize, args: [u8; 3]) -> usize {
    let Some(rest) = buf.get(from..) else {
        return from;
    };
    for bytes in rest.as_chunks::<32>().0 {
        let found = mask256::<KIND>(load256(bytes), args);
        if found != 0 {
            return from + found.trailing_zeros() as usize;
        }
        from += 32;
    }
    at_sse42::<KIND>(buf, from, args)
}

#[target_feature(enable = "avx512bw")]
fn at_avx512<const KIND: u8>(buf: &[u8], mut from: usize, args: [u8; 3]) -> usize {
    let Some(rest) = buf.get(from..) else {
        return from;
    };
    let (chunks, rest) = rest.as_chunks::<64>();
    for bytes in chunks {
        let found = mask512::<KIND>(load512(bytes), args);
        if found != 0 {
            return from + found.trailing_zeros() as usize;
        }
        from += 64;
    }
    if rest.is_empty() {
        return from;
    }
    let live = live(rest);
    let found = mask512::<KIND>(load512_masked(rest, live), args) & live;
    if found == 0 {
        buf.len()
    } else {
        from + found.trailing_zeros() as usize
    }
}
