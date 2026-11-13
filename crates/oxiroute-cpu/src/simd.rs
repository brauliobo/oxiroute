//! The vector kernels, one per level, behind [`dispatch!`].
//!
//! Everything `unsafe` of the crate is in this file, of two kinds:
//!
//! * calling a `#[target_feature]` kernel from the dispatchers: the level they receive never exceeds the hardware
//!   level ([`Level::clamp`], or [`crate::level`] which is lowered from it), so the CPU has every feature the kernel
//!   of that level enables;
//! * loading a vector register: from a reference to an array (all bytes readable, unaligned loads) or, for AVX-512,
//!   masked so that no lane past the slice is read.
#![allow(unsafe_code)]

use crate::{Level, TokenRun, is_tchar, scalar};
use core::arch::x86_64::*;

/// Calls the arm of `$level`; the scalar arm is the answer for [`Level::Scalar`].
macro_rules! dispatch {
    ($level:expr, $scalar:expr, sse42: $sse42:expr, avx2: $avx2:expr, avx512: $avx512:expr) => {
        match $level {
            // SAFETY: see the module docs: `$level` is at most the hardware level.
            Level::Avx512 => unsafe { $avx512 },
            Level::Avx2 => unsafe { $avx2 },
            Level::Sse42 => unsafe { $sse42 },
            Level::Scalar => $scalar,
        }
    };
}

pub(crate) fn control_at(level: Level, buf: &[u8], i: usize) -> usize {
    dispatch!(level, scalar::control_at(buf, i),
        sse42: control_at_sse42(buf, i), avx2: control_at_avx2(buf, i), avx512: control_at_avx512(buf, i))
}

pub(crate) fn token_run(level: Level, buf: &[u8]) -> TokenRun {
    dispatch!(level, scalar::token_run(buf),
        sse42: token_run_sse42(buf), avx2: token_run_avx2(buf), avx512: token_run_avx512(buf))
}

fn chunk<const N: usize>(buf: &[u8], i: usize) -> Option<&[u8; N]> {
    buf.get(i..)?.first_chunk()
}

/// The first `n` bits, `n <= 64`.
fn low_bits(n: u32) -> u64 {
    u64::MAX.checked_shr(64 - n).unwrap_or(0)
}

#[inline]
#[target_feature(enable = "sse2")]
fn load128(bytes: &[u8; 16]) -> __m128i {
    // SAFETY: 16 readable bytes, unaligned load.
    unsafe { _mm_loadu_si128(bytes.as_ptr().cast()) }
}

#[inline]
#[target_feature(enable = "avx2")]
fn load256(bytes: &[u8; 32]) -> __m256i {
    // SAFETY: 32 readable bytes, unaligned load.
    unsafe { _mm256_loadu_si256(bytes.as_ptr().cast()) }
}

#[inline]
#[target_feature(enable = "avx512bw")]
fn load512(bytes: &[u8; 64]) -> __m512i {
    // SAFETY: 64 readable bytes, unaligned load.
    unsafe { _mm512_loadu_si512(bytes.as_ptr().cast()) }
}

/// The first `live.count_ones()` bytes of `rest` (`live` is the low bits), zero elsewhere.
#[inline]
#[target_feature(enable = "avx512bw")]
fn load512_masked(rest: &[u8], live: u64) -> __m512i {
    // SAFETY: `live` has no bit at or past `rest.len()` and a masked-off lane is not read.
    unsafe { _mm512_maskz_loadu_epi8(live, rest.as_ptr().cast()) }
}

/// The live lanes of a 64-byte step: `rest.len()` of them, at most 64.
fn live(rest: &[u8]) -> u64 {
    low_bits(rest.len().min(64) as u32)
}

#[target_feature(enable = "sse4.2,popcnt")]
fn control_at_sse42(buf: &[u8], mut i: usize) -> usize {
    while let Some(bytes) = chunk(buf, i) {
        let x = load128(bytes);
        let ctl = _mm_cmpeq_epi8(_mm_min_epu8(x, _mm_set1_epi8(0x1f)), x);
        let found = _mm_movemask_epi8(_mm_or_si128(ctl, _mm_cmpeq_epi8(x, _mm_set1_epi8(0x7f))));
        if found != 0 {
            return i + found.trailing_zeros() as usize;
        }
        i += 16;
    }
    scalar::control_at(buf, i)
}

#[target_feature(enable = "avx2,bmi1,bmi2")]
fn control_at_avx2(buf: &[u8], mut i: usize) -> usize {
    while let Some(bytes) = chunk(buf, i) {
        let x = load256(bytes);
        let ctl = _mm256_cmpeq_epi8(_mm256_min_epu8(x, _mm256_set1_epi8(0x1f)), x);
        let found = _mm256_movemask_epi8(_mm256_or_si256(
            ctl,
            _mm256_cmpeq_epi8(x, _mm256_set1_epi8(0x7f)),
        ));
        if found != 0 {
            return i + found.trailing_zeros() as usize;
        }
        i += 32;
    }
    scalar::control_at(buf, i)
}

/// Lanes of `x` below 0x20 or DEL.
#[inline]
#[target_feature(enable = "avx512bw")]
fn controls512(x: __m512i) -> u64 {
    _mm512_cmple_epu8_mask(x, _mm512_set1_epi8(0x1f))
        | _mm512_cmpeq_epi8_mask(x, _mm512_set1_epi8(0x7f))
}

#[target_feature(enable = "avx512bw,bmi1,bmi2")]
fn control_at_avx512(buf: &[u8], mut i: usize) -> usize {
    while let Some(bytes) = chunk(buf, i) {
        let found = controls512(load512(bytes));
        if found != 0 {
            return i + found.trailing_zeros() as usize;
        }
        i += 64;
    }
    let Some(rest) = buf.get(i..).filter(|rest| !rest.is_empty()) else {
        return i;
    };
    let live = live(rest);
    let found = controls512(load512_masked(rest, live)) & live;
    if found != 0 {
        i + found.trailing_zeros() as usize
    } else {
        buf.len()
    }
}

/// Nibble tables of the token set: a byte is a token when `LO[byte & 15] & HI[byte >> 4] != 0`. Each high nibble of
/// `0x2_..=0x7_` owns one bit, which the low nibbles that make a token with it carry.
static LO: [u8; 16] = {
    let mut table = [0; 16];
    let mut low = 0;
    while low < 16 {
        let mut row = 2;
        while row < 8 {
            table[low as usize] |= if is_tchar(row << 4 | low) {
                1 << (row - 2)
            } else {
                0
            };
            row += 1;
        }
        low += 1;
    }
    table
};
static HI: [u8; 16] = [0, 0, 1, 2, 4, 8, 16, 32, 0, 0, 0, 0, 0, 0, 0, 0];

/// Per-lane answers of one step, one bit per lane: not a token, an ASCII capital, an underscore.
struct Masks {
    stop: u64,
    upper: u64,
    under: u64,
}

impl TokenRun {
    /// Takes the lanes of a `width`-lane step up to its first stop lane; true when the run ended in it.
    fn step(&mut self, masks: &Masks, width: u32) -> bool {
        let n = masks.stop.trailing_zeros().min(width);
        let taken = low_bits(n);
        self.len += n as usize;
        self.upper |= masks.upper & taken != 0;
        self.underscore |= masks.under & taken != 0;
        n < width
    }

    fn then(mut self, tail: Self) -> Self {
        self.len += tail.len;
        self.upper |= tail.upper;
        self.underscore |= tail.underscore;
        self
    }
}

fn bits(mask: i32) -> u64 {
    u64::from(mask.cast_unsigned())
}

#[inline]
#[target_feature(enable = "sse4.2")]
fn masks128(x: __m128i) -> Masks {
    let nibble = _mm_set1_epi8(0x0f);
    let lo = _mm_shuffle_epi8(load128(&LO), _mm_and_si128(x, nibble));
    let hi = _mm_shuffle_epi8(load128(&HI), _mm_and_si128(_mm_srli_epi16::<4>(x), nibble));
    let rel = _mm_sub_epi8(x, _mm_set1_epi8(b'A'.cast_signed()));
    Masks {
        stop: bits(_mm_movemask_epi8(_mm_cmpeq_epi8(
            _mm_and_si128(lo, hi),
            _mm_setzero_si128(),
        ))),
        upper: bits(_mm_movemask_epi8(_mm_cmpeq_epi8(
            _mm_min_epu8(rel, _mm_set1_epi8(25)),
            rel,
        ))),
        under: bits(_mm_movemask_epi8(_mm_cmpeq_epi8(
            x,
            _mm_set1_epi8(b'_'.cast_signed()),
        ))),
    }
}

#[inline]
#[target_feature(enable = "avx2")]
fn masks256(x: __m256i) -> Masks {
    let nibble = _mm256_set1_epi8(0x0f);
    let table = |t: &[u8; 16]| _mm256_broadcastsi128_si256(load128(t));
    let lo = _mm256_shuffle_epi8(table(&LO), _mm256_and_si256(x, nibble));
    let hi = _mm256_shuffle_epi8(
        table(&HI),
        _mm256_and_si256(_mm256_srli_epi16::<4>(x), nibble),
    );
    let rel = _mm256_sub_epi8(x, _mm256_set1_epi8(b'A'.cast_signed()));
    Masks {
        stop: bits(_mm256_movemask_epi8(_mm256_cmpeq_epi8(
            _mm256_and_si256(lo, hi),
            _mm256_setzero_si256(),
        ))),
        upper: bits(_mm256_movemask_epi8(_mm256_cmpeq_epi8(
            _mm256_min_epu8(rel, _mm256_set1_epi8(25)),
            rel,
        ))),
        under: bits(_mm256_movemask_epi8(_mm256_cmpeq_epi8(
            x,
            _mm256_set1_epi8(b'_'.cast_signed()),
        ))),
    }
}

/// Lanes past the slice are zero, which is not a token: the run ends at the end of the slice by itself.
#[inline]
#[target_feature(enable = "avx512bw")]
fn masks512(x: __m512i) -> Masks {
    let nibble = _mm512_set1_epi8(0x0f);
    let table = |t: &[u8; 16]| _mm512_broadcast_i32x4(load128(t));
    let lo = _mm512_shuffle_epi8(table(&LO), _mm512_and_si512(x, nibble));
    let hi = _mm512_shuffle_epi8(
        table(&HI),
        _mm512_and_si512(_mm512_srli_epi16::<4>(x), nibble),
    );
    let rel = _mm512_sub_epi8(x, _mm512_set1_epi8(b'A'.cast_signed()));
    Masks {
        stop: _mm512_testn_epi8_mask(lo, hi),
        upper: _mm512_cmple_epu8_mask(rel, _mm512_set1_epi8(25)),
        under: _mm512_cmpeq_epi8_mask(x, _mm512_set1_epi8(b'_'.cast_signed())),
    }
}

#[target_feature(enable = "sse4.2,popcnt")]
fn token_run_sse42(buf: &[u8]) -> TokenRun {
    let mut run = TokenRun::default();
    let mut i = 0;
    while let Some(bytes) = chunk(buf, i) {
        if run.step(&masks128(load128(bytes)), 16) {
            return run;
        }
        i += 16;
    }
    run.then(scalar::token_run(&buf[i..]))
}

#[target_feature(enable = "avx2,bmi1,bmi2")]
fn token_run_avx2(buf: &[u8]) -> TokenRun {
    let mut run = TokenRun::default();
    let mut i = 0;
    while let Some(bytes) = chunk(buf, i) {
        if run.step(&masks256(load256(bytes)), 32) {
            return run;
        }
        i += 32;
    }
    run.then(scalar::token_run(&buf[i..]))
}

#[target_feature(enable = "avx512bw,bmi1,bmi2")]
fn token_run_avx512(buf: &[u8]) -> TokenRun {
    let mut run = TokenRun::default();
    let mut i = 0;
    while let Some(bytes) = chunk(buf, i) {
        if run.step(&masks512(load512(bytes)), 64) {
            return run;
        }
        i += 64;
    }
    let Some(rest) = buf.get(i..).filter(|rest| !rest.is_empty()) else {
        return run;
    };
    run.step(&masks512(load512_masked(rest, live(rest))), 64);
    run
}
